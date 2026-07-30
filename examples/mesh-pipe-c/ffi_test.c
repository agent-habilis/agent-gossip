/*
 * ffi_test — the C-side test of the mesh FFI.
 *
 * Runs three memberships in ONE process (each handle owns its own runtime and
 * endpoint) over a private loopback mesh, and checks the four things a C consumer
 * needs from the engine:
 *
 *   1. creating a mesh and joining it from another handle
 *   2. converging on the shared JSON state document, in both directions
 *   3. broadcasting a message to the whole mesh
 *   4. sending a message to one peer — and *only* that peer
 *
 * Exits 0 with a `ok N - …` line per scenario, or non-zero at the first failure
 * with the reason on stderr. `crates/agent-habilis-mesh-ffi/tests/c_suite.rs`
 * compiles and runs this, so it is part of `cargo test`.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include "mesh.h"

/* A loopback mesh needs a few seconds to form; a loaded CI host needs more. */
#define MESH_TIMEOUT_MS 90000
/* How long to wait for a frame that should arrive. */
#define RECV_TIMEOUT_MS 30000
/* How long to wait for a frame that should NOT arrive. */
#define SILENCE_MS 5000
#define POLL_INTERVAL_MS 250

static int scenario = 0;

static void ok(const char *what) {
  printf("ok %d - %s\n", ++scenario, what);
  fflush(stdout);
}

static void fatal(const char *what) {
  const char *why = mesh_last_error();
  fprintf(stderr, "not ok %d - %s: %s\n", scenario + 1, what,
          why ? why : "no error message");
  exit(1);
}

static long now_ms(void) {
  struct timespec spec;
  clock_gettime(CLOCK_MONOTONIC, &spec);
  return spec.tv_sec * 1000 + spec.tv_nsec / 1000000;
}

static void sleep_ms(long ms) { usleep((unsigned)ms * 1000); }

/* The two JSON readers share a shape, so polling can be written once. */
typedef long (*json_reader)(mesh_pipe *, char *, size_t);

/*
 * Read a JSON document into a fresh allocation. Two calls: the first asks how
 * long it is (the NULL-buffer convention), the second fills the buffer.
 */
static char *read_json(mesh_pipe *pipe, json_reader read, const char *what) {
  long needed = read(pipe, NULL, 0);
  if (needed < 0) {
    fatal(what);
  }
  char *json = malloc((size_t)needed + 1);
  if (!json) {
    fprintf(stderr, "not ok - out of memory for %ld bytes of JSON\n", needed);
    exit(1);
  }
  if (read(pipe, json, (size_t)needed + 1) < 0) {
    free(json);
    fatal(what);
  }
  return json;
}

/*
 * Poll a JSON document until it contains `needle`. Returns the document (the
 * caller frees it) or dies with the last value seen — a timeout here is a real
 * failure, and the document is the evidence.
 */
static char *wait_for_json(mesh_pipe *pipe, json_reader read, const char *needle,
                           const char *what) {
  long deadline = now_ms() + MESH_TIMEOUT_MS;
  char *json = NULL;
  do {
    free(json);
    json = read_json(pipe, read, what);
    if (strstr(json, needle)) {
      return json;
    }
    sleep_ms(POLL_INTERVAL_MS);
  } while (now_ms() < deadline);

  fprintf(stderr, "not ok %d - %s: %s never appeared. Last document:\n%s\n",
          scenario + 1, what, needle, json);
  free(json);
  exit(1);
}

static mesh_pipe *open_or_die(mesh_opts *opts, const char *what) {
  mesh_pipe *pipe = mesh_open(opts);
  if (!pipe) {
    fatal(what);
  }
  return pipe;
}

/* Receive one frame that must arrive, into a caller-owned buffer. */
static void recv_or_die(mesh_pipe *pipe, unsigned char *buf, size_t cap,
                        mesh_frame *frame, const char *what) {
  long got = mesh_recv(pipe, buf, cap, RECV_TIMEOUT_MS, frame);
  if (got < 0) {
    fatal(what);
  }
  if (got == 0) {
    fprintf(stderr, "not ok %d - %s: timed out waiting for the frame\n",
            scenario + 1, what);
    exit(1);
  }
}

static void expect(int condition, const char *what) {
  if (!condition) {
    fprintf(stderr, "not ok %d - %s\n", scenario + 1, what);
    exit(1);
  }
}

int main(void) {
  size_t cap = mesh_max_chunk();
  unsigned char *buf = malloc(cap);
  if (!buf) {
    fprintf(stderr, "not ok - out of memory for a %zu-byte buffer\n", cap);
    return 1;
  }

  printf("# agent-habilis-mesh %s, max frame payload %zu bytes\n",
         mesh_version(), cap);

  /* ---- 1. create + join ---------------------------------------------- */

  mesh_opts create_opts;
  memset(&create_opts, 0, sizeof create_opts);
  create_opts.nick = "alice";
  mesh_pipe *alice = open_or_die(&create_opts, "creating the mesh");

  const char *mesh_id_value = mesh_id(alice);
  expect(mesh_id_value != NULL && mesh_id_value[0] != '\0',
         "the created mesh has an id");
  printf("# mesh %s\n", mesh_id_value);

  mesh_opts join_opts;
  memset(&join_opts, 0, sizeof join_opts);
  join_opts.mesh = mesh_id_value;
  join_opts.nick = "bob";
  mesh_pipe *bob = open_or_die(&join_opts, "joining as bob");

  join_opts.nick = "carol";
  mesh_pipe *carol = open_or_die(&join_opts, "joining as carol");

  free(wait_for_json(alice, mesh_peers_json, "\"bob\"", "bob joins the roster"));
  free(wait_for_json(alice, mesh_peers_json, "\"carol\"",
                     "carol joins the roster"));
  free(wait_for_json(bob, mesh_peers_json, "\"alice\"",
                     "bob sees the creator"));
  ok("creates a mesh and two peers join it");

  /* ---- 2. shared JSON state ------------------------------------------ */

  if (mesh_state_merge(alice, "{\"owner\":\"alice\",\"phase\":\"c-ffi-one\"}") != 0) {
    fatal("merging state as alice");
  }
  free(wait_for_json(bob, mesh_state_json, "c-ffi-one",
                     "alice's state reaches bob"));

  /* Back the other way, to prove convergence rather than one-way push. */
  if (mesh_state_merge(bob, "{\"phase\":\"c-ffi-two\",\"seen_by\":\"bob\"}") != 0) {
    fatal("merging state as bob");
  }
  char *converged = wait_for_json(alice, mesh_state_json, "c-ffi-two",
                                  "bob's state reaches alice");
  /* The CRDT merges rather than clobbers: alice's own key must survive. */
  expect(strstr(converged, "\"owner\"") != NULL,
         "alice's own key survives bob's merge");
  free(converged);
  ok("syncs the shared JSON state document both ways");

  /* ---- 3. broadcast --------------------------------------------------- */

  const char *broadcast = "hello mesh, from alice";
  if (mesh_send(alice, NULL, (const unsigned char *)broadcast,
                strlen(broadcast)) != 0) {
    fatal("broadcasting from alice");
  }

  mesh_frame frame;
  recv_or_die(bob, buf, cap, &frame, "bob receives the broadcast");
  expect(frame.len == strlen(broadcast) &&
             memcmp(buf, broadcast, frame.len) == 0,
         "the broadcast payload arrives unchanged");
  expect(frame.directed == 0, "the broadcast is not marked directed");
  expect(strcmp(frame.nick, "alice") == 0, "the broadcast is attributed to alice");
  expect(frame.eof == 0, "the broadcast is not an end-of-stream marker");
  ok("broadcasts a message to the gossip");

  /* ---- 4. directed to one peer --------------------------------------- */

  const char *whisper = "psst, alice only";
  if (mesh_send(bob, "alice", (const unsigned char *)whisper,
                strlen(whisper)) != 0) {
    fatal("sending a directed message from bob");
  }

  recv_or_die(alice, buf, cap, &frame, "alice receives the directed message");
  expect(frame.len == strlen(whisper) && memcmp(buf, whisper, frame.len) == 0,
         "the directed payload arrives unchanged");
  expect(frame.directed == 1, "the directed frame is marked directed");
  expect(strcmp(frame.nick, "bob") == 0, "the directed frame is attributed to bob");

  /*
   * The point of "directed": a third member of the same mesh is never shown it.
   *
   * The check is "the whisper never reaches carol", not "nothing reaches carol".
   * She is also a recipient of the scenario-3 broadcast, and her copy of it may
   * still be in flight — draining her queue first only looked deterministic, and
   * a late broadcast arriving inside this window failed the test for the wrong
   * reason. So: consume whatever turns up for the whole window and assert only
   * that none of it is the whisper.
   */
  long deadline = now_ms() + SILENCE_MS;
  for (;;) {
    long remaining = deadline - now_ms();
    if (remaining <= 0) {
      break;
    }
    long leaked = mesh_recv(carol, buf, cap, (int)remaining, &frame);
    if (leaked < 0) {
      fatal("checking that carol was not sent the directed message");
    }
    if (leaked == 0) {
      break; /* the window closed with nothing further queued */
    }
    expect(!(frame.len == strlen(whisper) &&
             memcmp(buf, whisper, frame.len) == 0),
           "a directed message is not surfaced to other peers");
  }
  ok("sends a message to one peer only");

  /* ---- teardown ------------------------------------------------------ */

  if (mesh_close(carol) != 0) {
    fatal("carol leaving");
  }
  if (mesh_close(bob) != 0) {
    fatal("bob leaving");
  }
  if (mesh_close(alice) != 0) {
    fatal("alice leaving");
  }
  free(buf);

  printf("# all %d scenarios passed\n", scenario);
  return 0;
}
