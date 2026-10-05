# Third-party code in the module `models`

## Wowser

- Source: the implementation of the shader selection of WotLK by the Wowser project
  (<https://github.com/wowserhq/wowser>), as published on wowdev.wiki, *M2/.skin/WotLK shader
  selection* (revision 21406, 25 May 2016): `sub836980`, `sub837680`, `shaderNamesFromTable`,
  `shaderNamesFromSingleOpTable`, `shaderNamesFromMultiOpTable` and `shaderNamesFromOther`.
- Authors: Wowser Contributors.
- Licence: MIT, whose notice follows.
- Translated into `src/shaders.rs`: the shader of each batch from its blending, its coordinates
  and its combiners; the layers merged into their first; the names of the vertex and pixel
  shaders, as the number of a pixel shader and the coordinates of each texture.

```
MIT License

Copyright (c) 2012-2018 Wowser Contributors

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

## Read, nothing taken

- wowdev.wiki, *M2*, *M2/.skin* (its environment mapping), *M2/Rendering* (the formulas of the
  pixel shaders, the alpha of an element and its test), read for the facts of the format and of
  the client.
