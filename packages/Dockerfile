FROM oven/bun:alpine
WORKDIR /app
COPY . .
# dist/ is the document root and is gitignored, so the image has to build its
# own — and .dockerignore keeps a stale local one from being copied over it.
RUN bun install --frozen-lockfile && bun run build
EXPOSE 3000
CMD ["bun", "server/server.ts"]
