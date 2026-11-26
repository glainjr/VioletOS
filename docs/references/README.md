# References

Standards the code is written against, kept unmodified so that a section cited in a comment can be read offline.

| File | Document | Source | SHA-256 |
|---|---|---|---|
| `rfc2131.txt` | RFC 2131, Dynamic Host Configuration Protocol | https://www.rfc-editor.org/rfc/rfc2131.txt | `a043b705785b81762505ded4cf71d61392b2b1da2b4ce1e32c13bc984de5f4a5` |
| `rfc3396.txt` | RFC 3396, Encoding Long Options in DHCPv4 | https://www.rfc-editor.org/rfc/rfc3396.txt | `1093f376d3937e151cda05567f3b60ce29487c4c010ad77483001e70bfa49623` |
| `rfc4361.txt` | RFC 4361, Node-specific Client Identifiers for DHCPv4 | https://www.rfc-editor.org/rfc/rfc4361.txt | `84a37802e74aad730cabcbf5fa3051c8d02ad286c6c46a044b0491bb765637d4` |
| `rfc5227.txt` | RFC 5227, IPv4 Address Conflict Detection | https://www.rfc-editor.org/rfc/rfc5227.txt | `bfe6de915c68fddffa6bee54dd217a377d2c694c4233cef1696c936d4202ecd4` |
| `rfc6842.txt` | RFC 6842, Client Identifier Option in DHCP Server Replies | https://www.rfc-editor.org/rfc/rfc6842.txt | `fa5ee87eac46d97d8e4c6448e5588e9d5f87624622a45137de384873dbe87c4f` |
| `rfc7844.txt` | RFC 7844, Anonymity Profiles for DHCP Clients | https://www.rfc-editor.org/rfc/rfc7844.txt | `359b456e9be5acaa618d654aa10f0d330696e8c2cf9a09ca6c143df7881fa3c8` |

The phone's DHCP client is modelled from RFC 2131 and RFC 5227 in [`docs/formal/dhcp`](../formal/dhcp/README.md), which also says where the client stands against the later ones.
