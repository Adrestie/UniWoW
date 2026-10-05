# Third-party code in the module `assets`

## wow-mpq, of warcraft-rs

- Source: <https://github.com/wowemulation-dev/warcraft-rs>, the crate `wow-mpq` 0.7.0
  (`file-formats/archives/wow-mpq`), commit `627b3a0d99b8420cc7640b5f99aff18c878e1529`.
- Authors: Daniel S. Reichenbach and WoW Emulation Contributors.
- Licence: MIT or Apache-2.0, at the choice of whoever uses it; UniWoW takes it under MIT, whose
  notice follows.
- Copied into `src/mpq.rs`: the encryption table (`crypto/keys.rs`), the hash of names
  (`crypto/hash.rs`), the decryption of blocks (`crypto/decryption.rs`), the kinds of hash
  (`crypto/types.rs`) and the search of the hash table (`tables/hash.rs`); into `src/tests.rs`,
  the encryption of blocks (`crypto/encryption.rs`), to write the test archives.
- Adapted into `src/mpq.rs`: the reading of the header, of the tables and of the files
  (`archive.rs`), to read by position from any thread at once, each file into one allocation of
  its size, for the formats and the compression of the archives of 3.3.5a only; without `rayon`.

```
MIT License

Copyright (c) 2023-2026 Daniel S. Reichenbach and WoW Emulation Contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## wow.export

- Source: <https://github.com/Kruithne/wow.export>, `src/js/db/WDCReader.js`, commit
  `c2fd7bde36a712be78a5da896c995b84fbfa2545`.
- Authors: Kruithne and Marlamin.
- Licence: MIT, whose notice follows.
- Translated into `src/db2.rs`: the reading of the DB2 of versions WDC2, `1SLC`, WDC3 and WDC5
  (their header, sections, columns, pallets, lists of ids, copies, maps of offsets and strings),
  for the two columns of a table of paths only; WDC2 takes the offsets of its strings from their
  field, as WDC3 does, where wow.export reads them inline.

```
MIT License

Copyright (c) Kruithne <kruithne@gmail.com>
Copyright (c) Marlamin <marlamin@marlamin.com>

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## WoWDBDefs

- Source: <https://github.com/wowdev/WoWDBDefs>, the definitions of `Map`, `AreaTable`,
  `CreatureDisplayInfo` and `CreatureModelData` for the build 3.3.5.12340.
- Licence of the definitions: CC BY-SA 4.0.
- Taken into `src/dbc.rs`: the places of the columns read, and the count of the columns of each
  table; no file of the project is copied.

## Read, nothing taken

- WDC1 is read from the public description of the format, as DB2Gen writes it; nothing comes from
  DB2Gen nor from WarcraftXL, under GPL-3.
