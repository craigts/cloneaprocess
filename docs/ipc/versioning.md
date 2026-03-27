# IPC Versioning Strategy

This document defines how the IPC surface evolves without breaking Rust or Swift components.

## Layers of Versioning

1. C ABI version
2. XPC protocol version
3. Payload schema version

## 1) C ABI Version

- The C ABI is the most stable surface.
- Additive changes are preferred; remove or change signatures only on a major version bump.
- Expose a compile-time constant for the ABI version in the shim implementation:

```c
#define XPC_BRIDGE_ABI_VERSION 1
```

If the ABI must change, increment the major version and provide parallel symbols for one release window (example: `xpc_client_create_v2`).

## 2) XPC Protocol Version

- Protocol version is communicated via the `ping` response.
- Fields:
  - `protocol_version` (current)
  - `protocol_min` (minimum supported)
  - `capabilities` (array of optional feature flags)

Example `ping` payload:

```json
{
  "protocol_version": 1,
  "protocol_min": 1,
  "capabilities": ["ax_snapshot", "burst_frames"]
}
```

## 3) Payload Schema Version

- Every payload includes `v` in the envelope.
- The service must ignore unknown fields.
- Clients must be able to handle missing optional fields.

Rules:

- New optional fields: no version bump required.
- New required fields: bump `v` and keep compatibility shims for `v-1` for at least one minor release.
- Removed fields: keep parsing support for at least one minor release.

## Compatibility Rules

- If the client sends a higher `v` than the service supports, the service returns `UNSUPPORTED_VERSION`.
- If the service sends higher `v`, the client may reject or best-effort parse if compatible.
- Use `capabilities` for feature negotiation instead of branching on `v` for small changes.

## Migration Playbook

1. Introduce new fields as optional.
2. Add a capability flag when behavior changes.
3. After one release, allow the new field to become required.
4. Remove old behavior only after both sides support the new version.
