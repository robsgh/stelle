# Stelle

Stelle is a self-hosted dashboard for a home lab. It combines link cards with sandboxed, server-side Luau widgets.

![MVP](https://img.shields.io/badge/status-MVP-8b5cf6)

## Quick start

```sh
docker build -t stelle .
docker run --rm --name stelle -p 8080:8080 stelle
```

Open <http://localhost:8080>.

The image includes the example configuration from `config-example/`. Stelle does not provide authentication. Keep it on a trusted network or put it behind an authenticated reverse proxy.

## Configuration

Stelle reads `/config/dashboard.yaml` when it starts. To use your own configuration, copy the example and mount it into the container:

```sh
cp -r config-example/. config/
docker run --rm --name stelle -p 8080:8080 \
  -v "$PWD/config:/config:ro" stelle
```

Git ignores `config/`, so it can contain private host names and settings. Restart Stelle after a configuration change.

A configuration requires a `widgets` list. It can also contain these settings:

- `title`: Replaces the time-based greeting.
- `subtitle`: Replaces the local time.
- `theme`: One of `system`, `light`, or `dark`. The default is `system`.
- `accent`: The main color. The default is `#8b5cf6`.

Without a title or subtitle, Stelle shows a local-time greeting and the current time.

### Link widgets

```yaml
widgets:
  - type: link
    label: Router
    description: Network administration
    url: https://192.168.1.1:8443
    accent: "#38bdf8"
```

Link URLs must use HTTP or HTTPS. The browser requests `/favicon.ico` from each link host.

### Luau widgets

A Luau widget gets data on the server and returns a statistics card. Each network origin must be present in `network_allow`.

```yaml
widgets:
  - type: lua
    id: repository
    script: widgets/github-stats.luau
    network_allow:
      - https://api.github.com
    settings:
      repository: robsgh/stelle
```

The script can use:

- `settings`: Read-only widget settings.
- `http.get(url, headers?)`: Sends an HTTP GET request to an allowed origin and returns `{ status, body }`.
- `json.decode(value)` and `json.encode(value)`: JSON conversion.
- `log(message)`: Server logging.

The script must return this structure:

```lua
return {
    title = "Widget title",
    subtitle = "Optional detail",
    href = "https://example.com",
    metrics = {
        { label = "Healthy", value = 12 },
        { label = "Offline", value = 1 }
    },
    fetched_at = os.date("!%Y-%m-%dT%H:%M:%SZ")
}
```

Each refresh uses a new sandbox with memory, execution-time, response-size, and network limits. Widgets cannot access the file system, processes, environment variables, persistent storage, or raw sockets.

## Compose deployment

`compose.yaml` deploys a prebuilt image behind Traefik. It expects an external Docker network and the required `STELLE_*` values in an untracked `.env` file.

```sh
docker compose up --detach
```

## Local development

Build the frontend:

```sh
cd frontend
npm ci
npm run build
cd ..
```

Start the backend:

```sh
STELLE_CONFIG="$PWD/config/dashboard.yaml" \
STELLE_STATIC_DIR="$PWD/frontend/build" \
cargo run
```

For frontend hot reload, run `npm run dev` in `frontend/`. Vite sends API requests to port `8080`.

Run the checks with `cargo test`, `npm run check`, and `npm run build`.

## Environment variables

- `STELLE_CONFIG`: Configuration file path. The default is `/config/dashboard.yaml`.
- `STELLE_STATIC_DIR`: Frontend file directory. The default is `/app/public`.
- `STELLE_PORT`: HTTP port. The default is `8080`.
- `RUST_LOG`: Rust log filter.

## HTTP API

- `GET /api/dashboard`: Public dashboard configuration.
- `POST /api/widgets/{id}/refresh`: Refreshes one Luau widget.
- `GET /healthz`: Health status.

## License

[MIT](LICENSE)
