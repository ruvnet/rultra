# Vendored contract schemas

Copied verbatim from `cognitum-one/cognitum-media`, `contracts/v0/schemas/`,
at commit `a991aef57a20dffcdc092019b34a4b85ad3b24ab`.

**Do not edit these files.** They are the other system's contract, not ours.
They are vendored rather than fetched at build time so that a build is
reproducible offline and so that a contract change upstream shows up as a
reviewable diff here instead of silently altering what we emit.

Re-sync with:

```sh
gh api repos/cognitum-one/cognitum-media/contents/contracts/v0/schemas/<name>.schema.json \
  --jq .content | base64 -d > crates/rultra-media/contracts/<name>.schema.json
```
