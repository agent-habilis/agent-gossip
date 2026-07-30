/*
 * mesh.h — the C ABI of the agent-habilis-mesh engine.
 *
 * Hand-written, and the counterpart of `src/ffi.rs`: change one, change the
 * other. `tests/ffi_smoke.rs` exercises the same entry points from Rust, and
 * `examples/mesh-pipe-c` compiles against this file, so a mismatch fails a test
 * rather than corrupting a caller's stack.
 *
 * Link against the cdylib the crate builds:
 *
 *     cargo build -p agent-habilis-mesh-ffi
 *     cc prog.c -I<repo>/crates/agent-habilis-mesh-ffi/include \
 *        -L<repo>/target/debug -lagent_habilis_mesh_ffi
 *
 * Conventions
 * -----------
 * - A pointer-returning call yields NULL on failure; an `int` call returns 0 on
 *   success and -1 on failure. `mesh_last_error()` then holds the reason.
 * - Every call is blocking and thread-confined: one handle belongs to one
 *   thread. Distinct handles are independent (a single process may hold several,
 *   each with its own runtime and endpoint).
 * - A handle from mesh_open() must be released with mesh_close().
 */
#ifndef AGENT_HABILIS_MESH_H
#define AGENT_HABILIS_MESH_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* An opaque live membership. */
typedef struct mesh_pipe mesh_pipe;

/*
 * How to reach a mesh. Zero-initialize, then set at most one selector:
 *
 *   - `mesh`  — join this 💬… id.
 *   - `topic` — derive a public mesh from a shared string (all peers passing the
 *               same string land in the same mesh).
 *   - neither — create a fresh mesh; the discovery flags below say how far it
 *               reaches, and mesh_id() returns the minted id.
 *
 * String fields are NUL-terminated or NULL. `max_peers == 0` takes the engine's
 * default active-view cap.
 */
typedef struct {
  const char *mesh;
  const char *topic;
  const char *nick; /* NULL mints a random nickname */
  int is_public;    /* create a public mesh (the all-on discovery preset) */
  int mdns;         /* discoverable over mDNS */
  int dht;          /* discoverable over the mainline DHT */
  int relay;        /* reachable via the default relay ladder */
  size_t max_peers;
} mesh_opts;

/*
 * Metadata for one received frame. The payload lands in the caller's buffer;
 * `len` says how many bytes were written there.
 */
typedef struct {
  char nick[64];  /* the author's nickname */
  int directed;   /* 1 = addressed to us specifically, 0 = broadcast */
  int eof;        /* 1 = an end-of-stream marker; `len` is then 0 */
  size_t len;
} mesh_frame;

/* Create or join a mesh. NULL on failure. */
mesh_pipe *mesh_open(const mesh_opts *opts);

/* Identity of this membership, borrowed for the handle's lifetime. */
const char *mesh_id(const mesh_pipe *handle);
const char *mesh_nickname(const mesh_pipe *handle);

/*
 * Send `len` bytes. `to` NULL broadcasts to the whole mesh; otherwise it is a
 * peer nickname and only that peer receives the frame. Payloads larger than
 * mesh_max_chunk() are split across frames automatically.
 */
int mesh_send(mesh_pipe *handle, const char *to, const uint8_t *buf, size_t len);

/* Send the end-of-stream marker, so a reader knows the stream is finished. */
int mesh_send_eof(mesh_pipe *handle, const char *to);

/*
 * Take the next inbound frame, waiting up to `timeout_ms`. Returns 1 with `out`
 * filled, 0 on timeout, -1 on failure. An end-of-stream marker arrives as 1 with
 * `out->eof` set — it is not signalled through the return value.
 *
 * `cap` must be at least mesh_max_chunk(); a frame too large for the buffer is
 * an error, never a silent truncation.
 */
long mesh_recv(mesh_pipe *handle, uint8_t *buf, size_t cap, int timeout_ms,
               mesh_frame *out);

/*
 * The shared state document — a JSON object every peer in the mesh converges on
 * (a CRDT underneath, so concurrent writers merge instead of clobbering).
 *
 * mesh_state_merge() applies an RFC 7386 merge document: present keys are set,
 * keys set to null are removed, everything else is left alone.
 *
 * mesh_state_json() writes the merged document into `buf` and returns the length
 * it needs, excluding the NUL. When that does not fit in `cap`, nothing is
 * written — retry with a bigger buffer. -1 on failure.
 */
int mesh_state_merge(mesh_pipe *handle, const char *json);
long mesh_state_json(mesh_pipe *handle, char *buf, size_t cap);

/* The live peer roster as JSON. Same length/`cap` convention as above. */
long mesh_peers_json(mesh_pipe *handle, char *buf, size_t cap);

/*
 * How many peers other than you are in the mesh right now; 0 means you are
 * alone. -1 on failure.
 *
 * Deliberately NOT the roster JSON's own "count" field, which includes you and
 * so never reads 0. This is the question a sender waiting for company has, and
 * the answer it can loop on.
 */
long mesh_peer_count(mesh_pipe *handle);

/*
 * Leave the mesh and free the handle. The handle is freed even when this returns
 * -1, so it must not be used again. Passing NULL is a no-op that returns 0.
 */
int mesh_close(mesh_pipe *handle);

/*
 * Why the most recent call on this thread failed, or NULL if it succeeded.
 * Borrowed until this thread's next mesh_* call, so copy it to keep it.
 */
const char *mesh_last_error(void);

/* The engine's build version stamp. Borrowed for the process's lifetime. */
const char *mesh_version(void);

/* The largest payload a single frame carries — the minimum safe mesh_recv() buffer. */
size_t mesh_max_chunk(void);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* AGENT_HABILIS_MESH_H */
