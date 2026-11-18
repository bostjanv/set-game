# syntax=docker/dockerfile:1

FROM node:22-slim AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

FROM rust:1-slim-trixie AS server
WORKDIR /server
COPY server/ ./
RUN cargo build --release

FROM debian:trixie-slim
RUN useradd --system --create-home set && mkdir /data && chown set /data
COPY --from=server /server/target/release/set-server /usr/local/bin/set-server
COPY --from=web /web/dist /app/web
USER set
ENV SET_DB=/data/set.db SET_STATIC=/app/web SET_ADDR=0.0.0.0:8080
VOLUME /data
EXPOSE 8080
CMD ["set-server"]
