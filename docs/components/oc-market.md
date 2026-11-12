# oc-market

`oc-market` provides the shared plugin catalog and installer policy. The GUI
and `ocp plugin install` use the same catalog selection and download checks.

| Module | Responsibility |
|---|---|
| `catalog` | Select the artifact for the requested platform and plugin format, or report a typed refusal. |
| `index` | Verify signed index manifests against the pinned public key. |
| `verify` | Check downloaded bytes against the size and SHA-256 digest in the catalog. |
| `install` | Select the user plugin directory, validate archive paths and limits, and extract plugin bundles. |
| `loopback` | Serve local fixture catalogs and archives for installer tests. |

The applications own download transport, user interaction, and reporting.
Keep artifact selection and verification here so both entry points apply the
same rules. See [Plugin index](../plugin-index.md) for the signed catalog
format.
