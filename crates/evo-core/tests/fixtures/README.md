# Synthetic firmware-update key

`update-test-key.der` is a locally generated RSA-3072 PKCS#1 **test-only private key**.
It signs synthetic instructions in the boot activation unit tests. It is public test
data, has no relationship to TI keys or images, and must never protect real data.

It was generated with .NET `RSA.Create(3072).ExportRSAPrivateKey()`. Keeping this tiny
fixture avoids expensive RSA key generation on every debug test run. Tests compute
their image hashes and signatures at runtime; no firmware payload is stored here.
