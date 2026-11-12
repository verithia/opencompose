# Compatibility and originality

OpenCompose supports third-party instruments and effects through CLAP, VST3,
VST2, AU and LV2. It also imports several foreign preset formats.

Compatibility means that a preset loads, its parameters map to the right
controls, and the result sounds close. It does not mean copying another
product's implementation. Our devices are independent implementations with our
own parameter names, structure and measured targets.

Code and in-repository documentation describe our design in our own terms. They
do not record reverse-engineering artifacts, reference-product constants,
decompiled names, addresses, offsets, or comparison scores. Measurements belong
to our own regression suite and are expressed against our own targets.

A foreign preset format may be named where the name is required to select a
file. A hosted plugin may be named where the name is required to identify a
tested host or a compatibility row. Those names are compatibility information,
not design provenance.

External design notes stay outside this repository. Do not add a provenance
table here.
