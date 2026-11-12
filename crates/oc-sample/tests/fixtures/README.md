# Sample provider test fixtures

Tests use recorded or schema-based responses without contacting any service.

| Fixture | Documented endpoint |
| --- | --- |
| Openverse search | Audio search |
| Internet Archive search and metadata | Advanced Search and Metadata API |
| Wikimedia Commons search and files | MediaWiki Action API |
| Freesound search and sound | API v2, requires a token for live requests |
| Jamendo search | Tracks API, requires a client ID for live requests |

The Openverse, Wikimedia Commons, and Internet Archive fixtures retain
metadata used to exercise the parsers, including missing optional fields.

The Freesound and Jamendo fixtures are written from each API's documented
response schema, not recorded from an account: ids, names, users and URLs are
illustrative. They include the fields the parsers read, such as license,
duration, previews and the attribution name, and they mix free licences with
a non-commercial one (Freesound "Attribution NonCommercial", Jamendo `by-nc`)
so the licence rules see both.
