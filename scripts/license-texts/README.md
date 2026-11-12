# Standard license texts

`scripts/third-party-licenses.py` uses these texts for a dependency that
declares a license but ships no license file of its own. They are copied
unchanged from the SPDX License List data
(<https://github.com/spdx/license-list-data>, `text/<id>.txt`). A placeholder
copyright line is filled with the package's declared authors.

Add a file named `<SPDX id>.txt` here when the script reports a missing
standard text.

`vst3-sdk.txt` is the license of the VST 3 SDK
(<https://github.com/steinbergmedia/vst3sdk>, `LICENSE.txt`). The script adds
it to the `vst3` crate, whose bindings are generated from the SDK headers.
