# Vendored three.js

`three.min.js` r128, fetched verbatim from cdnjs.

Vendored rather than loaded from a CDN because this runs on an appliance that
may have no internet, and a screensaver that fails offline is not a screensaver.
It is served from the binary at `/vendor/three.min.js`, so the console has no
runtime network dependency at all.

sha256: 9274bbcec8d96168626c732b5d31c775aa8cfb7eaa0599bec0c175908a2c1ce2

Re-fetch with:

```sh
curl -sL https://cdnjs.cloudflare.com/ajax/libs/three.js/r128/three.min.js \
  -o crates/rultra-ui/ui/vendor/three.min.js
```
