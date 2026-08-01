/*
 * mesh-pipe-c — a Unix pipe over the gossip mesh, in C.
 *
 * The C twin of `examples/mesh-pipe`: same subcommands, same flags, same wire
 * frames. `listen` reads stdin and streams it over the mesh; `connect` joins and
 * writes every inbound byte to stdout, exiting when the sender signals
 * end-of-stream. Because both sides speak the same frames, either binary talks
 * to the other:
 *
 *     ./mesh-pipe-c listen < file          # prints the mesh id on stderr
 *     cargo run -p mesh-pipe -- connect <hash> > copy
 *
 * Everything mesh-related happens through <mesh.h>; this file holds no protocol
 * knowledge beyond "bytes in, bytes out".
 */
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include "mesh.h"

/*
 * How long `listen` waits for someone to join before giving up, and how often it
 * looks. A human needs time to copy the printed id into another terminal; two
 * minutes is generous without hanging a script forever.
 */
#define DEFAULT_WAIT_FOR_PEER_SECS 120
#define PEER_POLL_INTERVAL_MS 250

/* Set by the signal handler; the I/O loops check it between blocking calls. */
static volatile sig_atomic_t stop_requested = 0;

static void on_signal(int signum) {
  (void)signum;
  stop_requested = 1;
}

/*
 * Trap ctrl-c ourselves. The library deliberately installs no signal handlers
 * (it may be embedded in a host with its own), and without SA_RESTART a blocked
 * fread() returns instead of resuming, so the loop below notices.
 */
static void install_signal_handlers(void) {
  struct sigaction action;
  memset(&action, 0, sizeof action);
  action.sa_handler = on_signal;
  sigaction(SIGINT, &action, NULL);
  sigaction(SIGTERM, &action, NULL);
}

static void fail(const char *what) {
  const char *why = mesh_last_error();
  fprintf(stderr, "mesh-pipe-c: %s: %s\n", what, why ? why : "unknown error");
  exit(1);
}

static void usage(void) {
  fprintf(stderr,
          "mesh-pipe-c — a Unix pipe over the gossip mesh (C consumer of "
          "agent-habilis-mesh %s)\n\n"
          "Usage:\n"
          "  mesh-pipe-c listen  [--mesh HASH] [--topic STRING] [--public] "
          "[--mdns] [--dht] [--relay]\n"
          "                      [--to NICK] [--chunk N] [--nick NAME] "
          "[--max-peers N]\n"
          "                      [--wait-for-peer SECS]\n"
          "  mesh-pipe-c connect [HASH] [--topic STRING] [--nick NAME] "
          "[--max-peers N]\n"
          "                      [--idle-timeout SECS]\n\n"
          "listen reads stdin and streams it over the mesh; with no selector it\n"
          "creates a loopback mesh and prints the id a connect peer needs.\n"
          "It waits up to --wait-for-peer seconds (default %d) for someone to\n"
          "join before sending, because frames are not retained: a peer that\n"
          "arrives after they were sent can never be given them. Pass 0 to send\n"
          "immediately regardless.\n",
          mesh_version(), DEFAULT_WAIT_FOR_PEER_SECS);
}

/* One parsed command line. */
struct args {
  mesh_opts opts;
  const char *to;       /* --to: direct every frame at this peer */
  size_t chunk;         /* --chunk: raw bytes per frame */
  long idle_timeout;    /* --idle-timeout: seconds, 0 = wait forever */
  long wait_for_peer;   /* --wait-for-peer: seconds, 0 = do not wait */
};

static long now_ms(void) {
  struct timespec spec;
  clock_gettime(CLOCK_MONOTONIC, &spec);
  return spec.tv_sec * 1000 + spec.tv_nsec / 1000000;
}

/*
 * Parse `argv[from..argc)`. A bare argument is taken as the mesh id (so
 * `connect <hash>` works positionally, like the Rust binary). Returns 0 on success.
 */
static int parse_args(int argc, char **argv, int from, struct args *out) {
  for (int index = from; index < argc; index++) {
    const char *arg = argv[index];
    int has_value = index + 1 < argc;

#define TAKE_VALUE(target)                                                     \
  do {                                                                         \
    if (!has_value) {                                                          \
      fprintf(stderr, "mesh-pipe-c: %s needs a value\n", arg);                 \
      return 1;                                                                \
    }                                                                          \
    (target) = argv[++index];                                                  \
  } while (0)

    if (strcmp(arg, "--mesh") == 0) {
      TAKE_VALUE(out->opts.mesh);
    } else if (strcmp(arg, "--topic") == 0) {
      TAKE_VALUE(out->opts.topic);
    } else if (strcmp(arg, "--nick") == 0) {
      TAKE_VALUE(out->opts.nick);
    } else if (strcmp(arg, "--to") == 0) {
      TAKE_VALUE(out->to);
    } else if (strcmp(arg, "--public") == 0) {
      out->opts.is_public = 1;
    } else if (strcmp(arg, "--mdns") == 0) {
      out->opts.mdns = 1;
    } else if (strcmp(arg, "--dht") == 0) {
      out->opts.dht = 1;
    } else if (strcmp(arg, "--relay") == 0) {
      out->opts.relay = 1;
    } else if (strcmp(arg, "--chunk") == 0) {
      const char *value = NULL;
      TAKE_VALUE(value);
      out->chunk = (size_t)strtoul(value, NULL, 10);
    } else if (strcmp(arg, "--max-peers") == 0) {
      const char *value = NULL;
      TAKE_VALUE(value);
      out->opts.max_peers = (size_t)strtoul(value, NULL, 10);
    } else if (strcmp(arg, "--idle-timeout") == 0) {
      const char *value = NULL;
      TAKE_VALUE(value);
      out->idle_timeout = strtol(value, NULL, 10);
    } else if (strcmp(arg, "--wait-for-peer") == 0) {
      const char *value = NULL;
      TAKE_VALUE(value);
      out->wait_for_peer = strtol(value, NULL, 10);
    } else if (strcmp(arg, "--help") == 0 || strcmp(arg, "-h") == 0) {
      usage();
      exit(0);
    } else if (arg[0] == '-') {
      fprintf(stderr, "mesh-pipe-c: unknown flag %s\n", arg);
      return 1;
    } else if (out->opts.mesh == NULL) {
      out->opts.mesh = arg;
    } else {
      fprintf(stderr, "mesh-pipe-c: unexpected argument %s\n", arg);
      return 1;
    }

#undef TAKE_VALUE
  }
  return 0;
}

/* True when no selector was given, i.e. this invocation mints a new mesh. */
static int is_create(const struct args *args) {
  return args->opts.mesh == NULL && args->opts.topic == NULL;
}

static int run_listen(struct args *args) {
  mesh_pipe *pipe = mesh_open(&args->opts);
  if (!pipe) {
    fail("opening the mesh");
  }
  if (is_create(args)) {
    /*
     * Deliberately the Rust binary's exact prefix, not a C-specific one: this
     * line is the machine-readable handoff a `connect` peer (and the test
     * harness) parses, so the two binaries must be interchangeable there.
     */
    fprintf(stderr, "mesh-pipe: mesh %s\n", mesh_id(pipe));
    fflush(stderr);
  }

  /*
   * Wait for company before streaming. Without this the whole mesh lives for
   * one fread(): with a file on stdin that is under two seconds, so the id we
   * just printed is dead before a human can paste it anywhere. Waiting is the
   * only fix available — pipe frames are never retained by the engine, so a
   * peer that joins after they were sent cannot be backfilled.
   */
  if (args->wait_for_peer > 0 && mesh_peer_count(pipe) < 1) {
    fprintf(stderr, "mesh-pipe-c: waiting for a peer to join (%lds)…\n",
            args->wait_for_peer);
    fflush(stderr);
    long deadline = now_ms() + args->wait_for_peer * 1000;
    long peers = 0;
    while (!stop_requested && now_ms() < deadline) {
      peers = mesh_peer_count(pipe);
      if (peers < 0) {
        fail("counting peers");
      }
      if (peers > 0) {
        break;
      }
      usleep(PEER_POLL_INTERVAL_MS * 1000);
    }
    if (peers < 1) {
      /* Nothing was sent, so this is a failure, not a quiet success. */
      fprintf(stderr, "mesh-pipe-c: no peer joined in %lds; nothing was sent\n",
              args->wait_for_peer);
      mesh_close(pipe);
      return 1;
    }
    fprintf(stderr, "mesh-pipe-c: peer joined; streaming stdin\n");
    fflush(stderr);
  }

  size_t chunk = args->chunk > 0 ? args->chunk : mesh_max_chunk();
  unsigned char *buf = malloc(chunk);
  if (!buf) {
    fprintf(stderr, "mesh-pipe-c: out of memory for a %zu-byte buffer\n", chunk);
    mesh_close(pipe);
    return 1;
  }

  int status = 0;
  while (!stop_requested) {
    size_t read = fread(buf, 1, chunk, stdin);
    if (read > 0 && mesh_send(pipe, args->to, buf, read) != 0) {
      fail("sending a frame");
    }
    if (read < chunk) {
      /* fread blocks until the buffer fills, so a short read means EOF, an
       * error, or a signal — and a signal we asked for is not an error. */
      if (ferror(stdin) && !stop_requested) {
        perror("mesh-pipe-c: reading stdin");
        status = 1;
      }
      break;
    }
  }

  /* One end-of-stream marker, so the reader knows to finish. */
  if (status == 0 && mesh_send_eof(pipe, args->to) != 0) {
    fail("sending the end-of-stream marker");
  }

  free(buf);
  if (mesh_close(pipe) != 0) {
    fail("leaving the mesh");
  }
  return status;
}

static int run_connect(struct args *args) {
  if (is_create(args)) {
    fprintf(stderr, "mesh-pipe-c: connect needs a mesh id or --topic STRING\n");
    return 1;
  }

  mesh_pipe *pipe = mesh_open(&args->opts);
  if (!pipe) {
    fail("joining the mesh");
  }

  size_t cap = mesh_max_chunk();
  unsigned char *buf = malloc(cap);
  if (!buf) {
    fprintf(stderr, "mesh-pipe-c: out of memory for a %zu-byte buffer\n", cap);
    mesh_close(pipe);
    return 1;
  }

  /* Poll in one-second slices so a signal (or the idle cap) is noticed. */
  long idle_slices = 0;
  int status = 0;
  while (!stop_requested) {
    mesh_frame frame;
    long got = mesh_recv(pipe, buf, cap, 1000, &frame);
    if (got < 0) {
      fail("receiving a frame");
    }
    if (got == 0) {
      idle_slices++;
      if (args->idle_timeout > 0 && idle_slices >= args->idle_timeout) {
        fprintf(stderr, "mesh-pipe-c: no frames for %lds; giving up\n",
                args->idle_timeout);
        status = 1;
        break;
      }
      continue;
    }
    idle_slices = 0;
    if (frame.eof) {
      break;
    }
    if (frame.len > 0 && fwrite(buf, 1, frame.len, stdout) != frame.len) {
      perror("mesh-pipe-c: writing stdout");
      status = 1;
      break;
    }
    fflush(stdout);
  }

  free(buf);
  if (mesh_close(pipe) != 0) {
    fail("leaving the mesh");
  }
  return status;
}

int main(int argc, char **argv) {
  if (argc < 2) {
    usage();
    return 1;
  }
  if (strcmp(argv[1], "--help") == 0 || strcmp(argv[1], "-h") == 0) {
    usage();
    return 0;
  }
  if (strcmp(argv[1], "--version") == 0) {
    printf("mesh-pipe-c (agent-habilis-mesh %s)\n", mesh_version());
    return 0;
  }

  struct args args;
  memset(&args, 0, sizeof args);
  /* Zero means "do not wait", so the default has to be set before parsing. */
  args.wait_for_peer = DEFAULT_WAIT_FOR_PEER_SECS;
  if (parse_args(argc, argv, 2, &args) != 0) {
    return 1;
  }
  if (args.opts.mesh && args.opts.topic) {
    fprintf(stderr, "mesh-pipe-c: pass only one of a mesh id / --topic\n");
    return 1;
  }

  install_signal_handlers();

  if (strcmp(argv[1], "listen") == 0) {
    return run_listen(&args);
  }
  if (strcmp(argv[1], "connect") == 0) {
    return run_connect(&args);
  }
  fprintf(stderr, "mesh-pipe-c: unknown subcommand %s\n", argv[1]);
  usage();
  return 1;
}
