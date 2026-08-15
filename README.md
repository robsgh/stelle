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

Git ignores `config/`, so it can contain private host names and settings. Stelle detects changes to the dashboard file and Luau scripts. A valid change applies to the next page load or widget request. If a change is invalid, Stelle keeps the previous configuration and writes the error to the log.

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

### Traefik discovery

A Traefik widget discovers enabled `Host(...)` HTTP routers and expands them into ordinary link cards. Stelle reads Traefik's HTTP API; it does not need access to the Docker socket.

```yaml
widgets:
  - type: traefik
    id: traefik-services
    api_url: https://traefik.example.com
    cache_ttl: 300
    exclude_hosts:
      - stelle.example.com
```

The Traefik dashboard API must be enabled and reachable from Stelle. Discovery runs lazily on the first dashboard request, caches successful results for `cache_ttl` seconds, and does not poll. Disabled routers, `HostRegexp(...)` rules, duplicate hosts, excluded hosts, and links already configured elsewhere are omitted. If a refresh fails, Stelle keeps the last successful discovery result.

### Luau widgets

A Luau widget gets data on the server and returns a statistics card. Each network origin must be present in `network_allow`.

```yaml
widgets:
  - type: lua
    id: repository
    script: widgets/github-stats.luau
    cache_ttl: 300
    network_allow:
      - https://api.github.com
    settings:
      repository: robsgh/stelle
```

`cache_ttl` is the number of seconds a successful result remains cached after the first request. It defaults to 300 seconds and must be between 10 seconds and 24 hours. Page loads use the server cache, while the refresh button forces a new execution. Stelle does not poll widgets in the background.

After a cached result expires, the next page request refreshes it. If that attempt fails, Stelle returns the last successful value. Changing the dashboard configuration or a Luau script invalidates the cache.

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

#### Nearby aircraft

The bundled `nearby-aircraft.luau` widget reads a tar1090 or ultrafeeder aircraft feed directly:

```yaml
widgets:
  - type: lua
    id: nearby-aircraft
    script: widgets/nearby-aircraft.luau
    cache_ttl: 60
    network_allow:
      - https://planes.example.com
    settings:
      base_url: https://planes.example.com
      max_age: 60
```

The card shows one recent aircraft with its signal or distance, altitude, speed, heading, and vertical rate. It links directly to that aircraft in tar1090. By default, the widget selects the strongest recent aircraft with a position. Set `receiver_lat`, `receiver_lon`, and optionally `radius_nm` under `settings` to select the closest aircraft within a radius instead. Receiver coordinates stay in the server-side configuration and are not returned to the browser.

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

Run all release checks, including a container smoke test, from the repository root:

```sh
./scripts/verify.sh
```

The individual checks are `cargo fmt --all --check`, `cargo clippy`, `cargo test`, `npm audit --audit-level=moderate`, `npm run check`, and `npm run build`.

## Release process

Stelle releases are prepared and verified locally; the repository does not use hosted CI or publish images to a container registry. Verify a clean release commit and create the version tag:

```sh
./scripts/verify.sh
git status --short
git tag -a v0.1.0 -m "Stelle v0.1.0"
```

## Environment variables

- `STELLE_CONFIG`: Configuration file path. The default is `/config/dashboard.yaml`.
- `STELLE_STATIC_DIR`: Frontend file directory. The default is `/app/public`.
- `STELLE_PORT`: HTTP port. The default is `8080`.
- `RUST_LOG`: Rust log filter.

## HTTP API

- `GET /api/dashboard`: Public dashboard configuration.
- `GET /api/widgets/{id}`: Returns a cached widget result, refreshing it on demand after expiry.
- `POST /api/widgets/{id}/refresh`: Refreshes one Luau widget.
- `GET /healthz`: Health status.

## License

[MIT](LICENSE)
