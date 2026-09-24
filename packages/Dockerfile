FROM oven/bun:alpine
WORKDIR /app
COPY . .
# build-webapp.ts reads the crate version from ../../crates, outside this
# context; the named `crates` context supplies it at the same relative path.
COPY --from=crates agent-gossip/Cargo.toml /crates/agent-gossip/Cargo.toml
# dist/ is the document root and is gitignored, so the image has to build its
# own — and .dockerignore keeps a stale local one from being copied over it.
RUN bun install --frozen-lockfile && bun run build
EXPOSE 3000
CMD ["bun", "web/server.ts"]
