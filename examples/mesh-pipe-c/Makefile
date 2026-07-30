# Builds the two C programs against the engine's cdylib.
#
#   make            both binaries (builds the Rust cdylib first)
#   make check      run the FFI test
#   make PROFILE=release
#
# `-std=gnu11` rather than `-std=c11`: strict ANSI hides the POSIX declarations
# these programs use (sigaction, clock_gettime, usleep) behind feature macros.

ROOT := $(abspath ../..)
PROFILE ?= debug
LIBDIR ?= $(ROOT)/target/$(PROFILE)
LIBNAME := agent_habilis_mesh_ffi

CFLAGS ?= -std=gnu11 -O1 -g -Wall -Wextra -Wpedantic
CPPFLAGS += -I$(ROOT)/crates/agent-habilis-mesh-ffi/include
# An absolute rpath, so the binaries run from any working directory.
LDFLAGS += -L$(LIBDIR) -Wl,-rpath,$(LIBDIR)
LDLIBS += -l$(LIBNAME)

BINS := mesh-pipe-c ffi_test

.PHONY: all lib check clean

all: $(BINS)

# `-p` is load-bearing: the workspace pins default-members to the agent-gossip
# app, so a bare `cargo build` at the root never builds this crate.
lib:
	cd $(ROOT) && cargo build -p agent-habilis-mesh-ffi \
		$(if $(filter release,$(PROFILE)),--release,)

# `| lib` is order-only: the cdylib must exist first, but its timestamp does not
# decide whether the C sources need recompiling.
mesh-pipe-c: pipe.c | lib
	$(CC) $(CFLAGS) $(CPPFLAGS) -o $@ $< $(LDFLAGS) $(LDLIBS)

ffi_test: ffi_test.c | lib
	$(CC) $(CFLAGS) $(CPPFLAGS) -o $@ $< $(LDFLAGS) $(LDLIBS)

check: ffi_test
	./ffi_test

clean:
	rm -f $(BINS)
	rm -rf $(BINS:%=%.dSYM)
