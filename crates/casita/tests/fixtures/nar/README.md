These fixtures were produced by `nix-store --dump` from a file containing the
six bytes `hello\n`, with modes 0644 and 0755 respectively. They are independent
of Casita's encoder. Reproduce by writing those bytes, setting the mode, and
running `nix-store --dump PATH > hello.nar` (or `hello-executable.nar`).

SHA-256:

* hello.nar: 1c37d01af40be2e80691de3cc3df44377a699afbb17c68f080964b2fd071fc13
* hello-executable.nar: 65436039d3f93ca19a8dbf1c60b15739ed58f53f14b8d372acc1b351533010fa

The tree fixture contains `empty/`, `nested/hello` (executable, `hello\n`),
`nested/zero` (empty), `link -> ../nested/hello`, and an empty file named
with byte 0xff. It was also generated with `nix-store --dump`.

* tree.nar: 0ff00ea57506ae9afb2b4a8aa1f1dad978240c83a189bad2869497fe5b026fec
