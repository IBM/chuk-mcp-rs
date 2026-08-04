# Changelog

Notable changes to `chuk-mcp`. Versions follow [semver]; while the crate is
pre-1.0 a minor bump is where a breaking change may appear.

## [0.2.0] — 2026-08-04

Serving `2026-07-28` correctly, not just speaking it. `0.1.0` shipped the
client side of the revision and a server that answered its requests; this
release makes the server *enforce* the revision's obligations, which is a
different and larger thing. Verified against the official
`@modelcontextprotocol/conformance` suite at `2026-07-28` — all 40 server
scenarios and all 30 client scenarios, authorization included — now blocking in
CI alongside the `2025-11-25` runs.

### Added

- **OAuth 2.1 authorization** (`auth` feature, on by default), for HTTP
  transports. Protected-resource and authorization-server metadata discovery
  across every well-known layout the specification requires; Client ID Metadata
  Documents, pre-registration and Dynamic Client Registration in the
  specification's priority order, with `application_type` stated so an OIDC
  server does not reject the loopback redirect; PKCE `S256`; the `resource`
  parameter on both requests; RFC 9207 `iss` validation in all four of its
  cases; scope selection from the challenge or `scopes_supported`, with the
  union-preserving step-up and a retry limit; the three token-endpoint auth
  methods; `offline_access` and refresh; and credentials keyed by issuer, so an
  authorization-server change re-registers rather than reusing an identity that
  server never issued.

  The library does everything except put the authorization page in front of a
  person: that is [`AuthorizationHandler`], one method. The loopback listener,
  the `state` and `iss` checks and the code exchange are handled here. Tokens
  are held in memory and never written to disk — implement [`TokenStore`] to
  decide where they belong.

  Verified against all 23 non-extension authorization scenarios of the official
  conformance suite, at both `2026-07-28` and `2025-11-25`.
- **Caching hints on cacheable results** (SEP-2549). `server/discover`,
  `tools/list`, `prompts/list`, `resources/list`, `resources/templates/list`
  and `resources/read` now carry the `ttlMs` and `cacheScope` that the
  specification requires. Configurable per operation through
  [`CachePolicy`]; the default is conservative — `cacheScope: "private"`
  everywhere, because `"public"` lets a shared gateway serve one caller's
  result to another and only the server knows whether that is safe.
  `CacheHints::read` reads them back on the client side.
- **`subscriptions/listen`** (SEP-2575), replacing the HTTP `GET` endpoint and
  `resources/subscribe`. A long-lived response stream with a per-request
  notification filter, the `notifications/subscriptions/acknowledged` message
  first, and every message tagged with
  `io.modelcontextprotocol/subscriptionId`. Servers announce changes through
  `McpServer::listeners()`.
- **`McpServer::register_tool_requiring`**, for a tool that cannot run without
  a declared client capability. A modern request that did not declare it is
  refused with `-32021` and `data.requiredCapabilities` before the handler
  runs.
- **`McpServer::register_raw_prompt`**, so `prompts/get` can answer with an
  `input_required` result rather than messages — one of the three requests the
  revision allows that on.
- **Multi round-trip fields on `CallContext`**: `input_responses()`,
  `input_response(key)`, `request_state()`, `client_capabilities()` and
  `client_supports()`. These travel beside `arguments` in the request params
  rather than inside them, so a handler had no way to reach them before.
- **`McpServer::with_request_state_validator`**, checking an echoed
  `requestState` before anything acts on it. With no session, the client has
  been holding what the server needs to remember, and the specification
  requires state failing integrity verification to be rejected.

### Fixed

Two defects in the authorization work itself, found by reviewing it rather
than by a failing test:

- **A stored token is no longer offered before the issuer is known.** The
  connection used to reuse any token held for the resource, whichever
  authorization server minted it — so a server that had since changed
  authorization servers would be sent a token the new one never issued, which
  the specification forbids outright. The optimisation is gone; the `401` that
  drives discovery costs one round trip and is always right.
- **The callback listener no longer treats the first connection as the
  answer.** A browser opening a page makes more connections than the one
  carrying the redirect — a favicon fetch, a speculative preconnect — and any
  could arrive first, so a `GET /favicon.ico` could be read as a failed
  authorization. It now ignores connections that carry no result.

`Auth::new()` also no longer defaults to a handler that fetches the
authorization URL from this process. That URL is named by metadata discovered
from the MCP server, so the convenient default would have let any server direct
an outbound request from the client to an address of its choosing. The default
now refuses and says what to supply; [`FollowRedirect`] remains available for
unattended clients, with the caveat documented.

Four client-side defects, all found by running the `2026-07-28` **client**
scenarios for the first time. The previous conformance runner drove only the
legacy era and called a single tool with empty arguments, which was enough to
hide every one of them.

- **`x-mcp-header` parameters are now actually promoted.** The machinery
  existed but nothing wired it into `call_tool`: the annotation lives in the
  tool's `inputSchema`, seen during `tools/list`, while the promotion has to
  happen on a later `tools/call`, and nothing carried the schema across that
  gap. The Streamable HTTP transports now remember listed schemas
  ([`ToolSchemas`]) and mirror the designated parameters into `Mcp-Param-*`.
- **`list_tools` excludes tools with invalid `x-mcp-header` annotations.**
  Required by the specification, and previously not done — a tool whose
  annotation named a header containing CR/LF was returned and callable.
- **`connect()` against a legacy HTTP server no longer loses server-initiated
  requests.** The era the handshake settled was never told to the transport,
  which independently concluded "modern" whenever a legacy server answered
  `server/discover` with an ordinary `200` + `-32601`. The visible symptom was
  a client that never opened the `GET` stream, so a pushed
  `elicitation/create` or `sampling/createMessage` never arrived.
- **`connect()` waits for that stream before returning.** Opening it costs a
  round trip the caller would otherwise outrun, losing anything the server
  pushed in between.

### Changed

- **Request headers are validated on the serve path** (SEP-2243). A
  `2026-07-28` request whose `Mcp-Method`, `Mcp-Name` or `Mcp-Param-*` headers
  disagree with its body is refused with `-32020` and HTTP `400` instead of
  being executed. This is the server's half of a bargain the client side
  already kept: the promotion exists so intermediaries can route without
  parsing the body, which is only safe if the two agree.
- **`params._meta` is required and checked.** A modern request missing `_meta`,
  `io.modelcontextprotocol/protocolVersion` or
  `io.modelcontextprotocol/clientCapabilities` is refused with `-32602` and
  HTTP `400`. `clientInfo` stays optional, as a SHOULD.
- **Removed methods are answered as removed.** For a `2026-07-28` request,
  `initialize`, `ping`, `logging/setLevel`, `resources/subscribe`,
  `resources/unsubscribe` and `notifications/roots/list_changed` return
  `-32601` with HTTP `404`. The same requests over a legacy connection are
  still served — era is a property of the request, not of the server.
- **No log notifications unless the request asked.** `notifications/message` is
  now emitted only for a modern request carrying
  `io.modelcontextprotocol/logLevel`, and only at or above that level. Legacy
  requests keep using the `logging/setLevel` floor, which is now actually
  applied rather than merely recorded.
- **`UnsupportedProtocolVersionError` echoes the requested version** in
  `data.requested` alongside `data.supported`, and arrives as HTTP `400`.
- Version bumped to `0.2.0`: `0.1.0` is published and cannot be replaced.

### Testing

- The in-repo rule suite grew from 42 rules to **60**. The areas added by this
  release had no rows before, which by the suite's own convention read as
  unimplemented: caching hints, `_meta` validation, removed methods, the
  unsupported-version echo, the four `subscriptions/listen` rules, and nine
  authorization rules covering discovery order, issuer and resource
  validation, the `iss` table, the no-normalisation requirement, the scope
  union, PKCE and challenge parsing.
- CI now builds, lints and tests with `--no-default-features`. `auth` is on by
  default, so nothing else would notice it breaking when switched off — which
  is exactly how the conformance runner broke during development.

### Python bindings

The server-side wire contract gained the 2026-07-28 obligations a Python
dispatcher has to meet: `missing_required_meta`, `is_removed_method`,
`removed_methods`, `stamp_cache_hints`, `subscription_filter` and
`subscription_acknowledgement`. Same code the Rust server runs, so there is
one implementation of the contract rather than two. See
[docs/python.md](docs/python.md).

### Notes

The authorization *extensions* — DPoP, client credentials, JWT bearer,
enterprise-managed — are separate optional specifications and are not
implemented, nor is `private_key_jwt` client authentication. Neither is the
`io.modelcontextprotocol/tasks` extension.

## [0.1.0] — 2026-07-18

First release. Client and server for MCP across both protocol generations —
the stateless `2026-07-28` revision and the legacy `2025-11-25` /
`2025-06-18` / `2025-03-26` / `2024-11-05` lifecycle — over stdio, Streamable
HTTP and the deprecated HTTP+SSE transport, with Python bindings.

[semver]: https://semver.org/
[`CachePolicy`]: https://docs.rs/chuk-mcp/latest/chuk_mcp/server/caching/struct.CachePolicy.html
[`ToolSchemas`]: https://docs.rs/chuk-mcp/latest/chuk_mcp/protocol/tool_schemas/struct.ToolSchemas.html
[`AuthorizationHandler`]: https://docs.rs/chuk-mcp/latest/chuk_mcp/auth/trait.AuthorizationHandler.html
[`FollowRedirect`]: https://docs.rs/chuk-mcp/latest/chuk_mcp/auth/struct.FollowRedirect.html
[`TokenStore`]: https://docs.rs/chuk-mcp/latest/chuk_mcp/auth/store/trait.TokenStore.html
