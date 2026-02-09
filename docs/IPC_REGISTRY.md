**IPC Registry and Lazy Endpoint Wiring**

This note describes the userspace registry service and the lazy subscription model
for IPC endpoint wiring.

**Registry Responsibilities**
- Store output metadata: `(service_name, endpoint_name) -> grant_endpoint`.
- Validate subscriptions (exists, basic checks).
- Relay grant requests to the producer via its `grant_endpoint`.
- Provide a simple list operation for discovery/debugging.

The registry does not store output endpoint tokens, only a grant endpoint that
the producer listens on to mint a transferable send token on demand.

**Protocol (Message Labels)**
- `REGISTRY_REGISTER`: register output metadata.
- `REGISTRY_UNREGISTER`: remove output metadata.
- `REGISTRY_LIST`: list registered outputs.
- `REGISTRY_SUBSCRIBE`: request subscription to `(service, endpoint)`.
- `REGISTRY_SUBSCRIBE_REPLY`: status (0 ok, negative error).
- `REGISTRY_GRANT_REQUEST`: registry -> producer grant request.
- `REGISTRY_GRANT_DELIVER`: producer -> requester granted token.

Payload format (for names):
```
u16 service_len | u16 endpoint_len | service_bytes | endpoint_bytes
```

**Grant Flow (ASCII)**
```
Requester                    Registry                    Producer
    |  SUBSCRIBE(svc, ep)       |                           |
    |-------------------------->|                           |
    |                           |  GRANT_REQUEST(ep, reply) |
    |                           |-------------------------->|
    |                           |                           | derive send token
    |                           |                           | send GRANT_DELIVER
    |                           |<--------------------------|
    |  GRANT_DELIVER(token)     |                           |
    |<--------------------------|                           |
```

The requester blocks on its registry control endpoint until it receives a
`GRANT_DELIVER` for the endpoint name.

**Security Assumptions**
- Token handles are unforgeable in practice (kernel-generated).
- Registry never holds output endpoint tokens, only producer grant endpoints.
- Producers retain authority to grant tokens (least privilege).

**Startup and Defaults**
- Each process is given a registry send token in `TOKEN_REGISTRY`.
- Each process is given a default capability token (`TOKEN_IPC`) with
  `CREATE` and IPC rights to create endpoints.
- Processes should call `registry::init("service_name")` and
  `registry::register_default_outputs()` at startup.
- Additional outputs are registered explicitly.

**Replacing Build-Time Wiring**
- Init no longer wires console/tty/kbd tokens at spawn time.
- Services register their outputs (e.g., `console:write`, `tty:main`).
- Consumers request subscriptions at runtime (lazy).
- Failure handling: if subscribe replies with an error, retry or backoff.

**Output Ordering Note**
- Some consumers (tty) may emit output before a console subscription exists.
  In that case, tty buffers a small amount of output until it can forward it.

**Lifecycle Notes**
- Registry removes entries on explicit unregister (future: on-exit cleanup).
- If a producer dies, tokens become invalid; consumers must re-subscribe.
