# Backend protocol

The backend listens at `$XDG_RUNTIME_DIR/omakindle/backend.sock`. The socket
and every message are private to the current user. Transport is UTF-8 JSON,
one object per line.

A request carries a caller-chosen integer id and a flattened command:

```json
{"v":1,"id":7,"command":"refresh"}
```

A successful response keeps that id:

```json
{"type":"response","v":1,"id":7,"ok":true,"result":{}}
```

Failures set `ok` to false and return a stable machine-readable error code plus
a human-readable message:

```json
{"type":"response","v":1,"id":7,"ok":false,"errorCode":"auth_expired","error":"Amazon signed this session out; paste fresh cookies"}
```

On connect the server pushes a complete snapshot:

```json
{"type":"snapshot","v":1,"state":{}}
```

and a `state_changed` event whenever the state changes:

```json
{"type":"event","v":1,"event":"state_changed","state":{}}
```

## State

The snapshot contains:

- `lifecycle`: `unconfigured`, `loading`, `ready`, or `error`
- `errorCode` and `error`: the stable code and a redacted message
- `region`: Amazon store region, for example `us` or `de`
- `books`: every book in the library, newest acquisition first, each with
  `asin`, `title`, `authors`, `coverUrl`, `webReaderUrl`, `resourceType`, and
  `originType`
- `reading`: books with progress, newest sync first, each with the book fields
  plus `percentageRead`, `deviceName`, and `syncTime` (Unix milliseconds)
- `updatedAt`: Unix seconds of the last successful refresh
- `refreshing`: true while a refresh is running
- `needsDeviceToken`: true when the stored session has cookies but no device
  token; the library and highlights work, progress is skipped until the token
  is stored

## Commands

- `hello` — returns `protocolVersion` and `backendVersion`
- `ping`
- `get_state`
- `refresh`
- `set_credentials` with `cookies`, `deviceToken`, and optional `region`; the
  values are validated against Amazon before they are stored owner-only at
  `~/.config/omakindle/session.json`
- `store_cookies` with `cookies` and optional `region`; stores a cookie-only
  session, sets `needsDeviceToken`, and skips progress polling
- `set_device_token` with `deviceToken`; adds the token to the stored session,
  registers the device, and refreshes
- `clear_credentials`
- `set_refresh_minutes` with `minutes` from 5 through 1440
- `get_highlights` with `asin`; returns `highlights` with `asin`, `count`,
  `limited` (Amazon truncates the web notebook), and `items` (`id`, `text`,
  `note`, `color`, `location`, `page`)

## Error codes

- `unconfigured` — no session stored yet
- `invalid_credentials` — cookies or device token were rejected
- `auth_expired` — the stored session was signed out
- `session_error` — the session file could not be read or written
- `busy` — a refresh is already running
- `network`, `parse`, `amazon_<status>` — transport, parsing, or Amazon failure
- `bad_request`, `unknown_command`, `unsupported_version`

## Notes

- Cookies are sent to Amazon together with the current `session-id` as
  `x-amzn-sessionid`; book requests also carry `x-adp-session-token` from the
  device registration.
- Requests use a Chrome 149 macOS TLS/HTTP2 profile because Amazon rejects
  clients whose fingerprint does not look like a browser. The client is
  pinned to `wreq` and `wreq-util`.
- The backend refreshes on demand, on a timer (`set_refresh_minutes`), and on
  startup when credentials exist. It keeps its last successful state in
  `~/.cache/omakindle/state.json` so the bar has something to show
  immediately after a restart.
