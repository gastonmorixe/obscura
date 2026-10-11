# Resolve: crates/obscura-net/src/wreq_client.rs

Merge: upstream/main into gm/private-upgrade-20261010.
Sources consulted: research/private-fixes-inventory.md, project memories mq1012m6-0ae8 / mq10j7xo-4f59 / mq2z4wqf-0fe8.

Proof: `Grep` for `<<<<<<<|=======|>>>>>>>||||||||` in this file returned no matches.

## Hunk 1: `with_proxy` signature (~line 216)

- **Ours (HEAD):** 2-arg `with_proxy`, comments about Chrome 148 identity, accept-encoding via emulation, no cache-control/pragma.
- **Upstream:** 3-arg `with_proxy(..., allow_private_network: bool)` for SSRF dns_resolver (#793).
- **Decision:** Take upstream signature. Keep private stealth comments. Body already used `allow_private_network` and `Self::new` already passes `false`.

## Hunk 2: redirect loop bound (~line 361)

- **Ours:** `for hop in 0..20` (needed `hop` for hop-0 nav headers).
- **Upstream:** `for _ in 0..=20` plus `validate_request_mode` each hop (Fetch-spec 20 redirects).
- **Decision:** Take upstream loop + validate. Nav-only headers now key off `request.mode == Navigate`, so `hop` is unused.

## Hunk 3: request header set (~line 373)

- **Ours:** Force `STEALTH_SEC_CH_UA` / `STEALTH_USER_AGENT` / `sec-ch-ua-platform`, UIR always, hop-0 accept+priority+sec-fetch-user. Simple `get_cookie_header`. No cache-control/pragma.
- **Upstream:** `ResourceRequest`-driven accept/sec-fetch-site/mode/dest, UIR+sec-fetch-user only for Navigate, referer, `serialized_request_origin`, credential-aware SameSite cookies.
- **Decision:** Merge both.
  - Keep private Chrome 148 version-identity overrides (Bloomberg/PerimeterX + WSJ/DataDome).
  - Keep no cache-control/pragma (DataDome).
  - Keep accept-encoding owned by emulation (not set here).
  - Take upstream ResourceRequest accept/sec-fetch/referer/origin/cookie path (post-conflict code already depends on `request_origin` and credentials).
  - On Navigate, also set `priority: u=0, i` from private complete nav headers.
  - Document accept for Document equals the private hop-0 Accept string (`client.rs` `ResourceRequest::accept`), so no hardcode needed.

## Kept private constants (unchanged, outside conflict hunks)

- `STEALTH_USER_AGENT` Chrome/148.0.0.0
- `STEALTH_SEC_CH_UA` 2-brand GREASE Chromium 148
- macOS platform / MacIntel navigator constants
- Emulation profile remains `Chrome145` + `Platform::MacOS` (freshest profile in wreq-util, version strings overridden to 148 on the wire)
