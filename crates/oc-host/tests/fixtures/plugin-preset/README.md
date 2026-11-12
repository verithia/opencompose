# Plugin preset fixtures

Synthetic `.ffp` files for parser coverage. They are not vendor files and carry
only the bytes each test needs.

| File | Purpose |
|---|---|
| `binary-eq-lowcut.ffp` | Binary EQ container with one active low-cut band. |
| `binary-eq-truncated.ffp` | First 100 bytes of the binary EQ fixture. |
| `binary-eq-bad-magic.ffp` | Binary fixture with an unknown magic value. |
| `binary-eq-length-mismatch.ffp` | Binary fixture with one extra value. |
| `binary-eq-normalised.ffp` | Binary fixture with a normalised value where log2(Hz) is required. |
| `binary-eq-version-3.ffp` | Binary fixture with an unsupported version. |
| `text-comp-clean.ffp` | Text container with a reduced compressor parameter set. |
| `text-comp-vocal.ffp` | Text container with a second reduced parameter set. |

Regenerate with:

```sh
cd crates/oc-host/tests/fixtures/plugin-preset
python3 make_fixtures.py
```
