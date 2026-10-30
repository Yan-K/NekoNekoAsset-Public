# Bundled Noto Sans TC, JP, SC and KR 2.004

NekoNekoAsset bundles the variable-weight Noto Sans TC, Noto Sans JP, Noto Sans SC
and Noto Sans KR fonts as frontend assets (`app/src/fonts/*.woff2`). The Vite build
copies them into the frontend bundle that is embedded in the application executable.
They are used as the CJK user-interface fonts; Appearance > Built-in fonts switches
back to the Windows system fonts.

(c) 2014-2021 Adobe (http://www.adobe.com/), with Reserved Font Name 'Source'.
The fonts are licensed under the SIL Open Font License, Version 1.1, preserved byte
for byte in `OFL.txt`. They are not covered by the application's MIT license and
are not sold separately. The shipped family names do not use the Reserved Font
Name 'Source'.

## Provenance

Distribution source: the Google Fonts repository at commit
`6157dfaa640ca5738b4d1a7070fe389c3036d7fc` (2026-10-29),
https://github.com/google/fonts/tree/6157dfaa640ca5738b4d1a7070fe389c3036d7fc/ofl .
Google Fonts records the upstream as https://github.com/notofonts/noto-cjk at commit
`523d033d6cb47f4a80c58a35753646f5c3608a78` (tag `Sans2.004`). All four files report
`Version 2.004-H2`.

`OFL.txt` is the original `ofl/notosanstc/OFL.txt` at that commit (Git blob
`1c9f43281b8f216c5461fe9ac729afbade7724e4`, 4388 bytes, SHA256
`1c05c68c34f9708415aada51f17e1b0092d2cea709bf4a94cd38114f9e73d7d9`). The
`OFL.txt` files of the JP, SC and KR directories are byte-identical to it.

| Original file at the commit | Git blob | Bytes | SHA256 |
| --- | --- | ---: | --- |
| `ofl/notosanstc/NotoSansTC[wght].ttf` | `82943579ad39c281212df8f14ac999824694a6ab` | 11941968 | `864727d210d54f2537bbe23b3a839436c3992af72de9322af5270897246bd44f` |
| `ofl/notosansjp/NotoSansJP[wght].ttf` | `cdd8f083c1f5928ff3361f8cda4d3fc9462cbe89` | 9589900 | `c2f3b4d463500a2ddcd3849cded1fceeb9fd6d1c32e6cbecd568453ba50fc68f` |
| `ofl/notosanssc/NotoSansSC[wght].ttf` | `fb0637bafbcd804fe32152370a1225990745b4bc` | 17772300 | `a3041811a78c361b1de50f953c805e0244951c21c5bd412f7232ef0d899af0da` |
| `ofl/notosanskr/NotoSansKR[wght].ttf` | `b386890ba945e1f39448a6b59f20c5d194f58808` | 10414588 | `194018e6b2b293a7964f037b25c0249ce1418bc9ab3c971060a03aa57861e252` |

## Format conversion

Each original file was repackaged as WOFF2 with fontTools 4.66.1 and Brotli 1.2.1
(`fontTools.ttLib.woff2.compress`). No outlines, metrics, names or OpenType tables
were edited. Decoding the WOFF2 files reproduces every table except `head`, where
only `checkSumAdjustment` and flags bit 11 differ, as the WOFF2 transform requires.
Under the OFL definitions a format change can make a Modified Version; it remains
under the OFL and does not use the Reserved Font Name.

| Shipped file | Bytes | SHA256 |
| --- | ---: | --- |
| `app/src/fonts/NotoSansTC-VF.woff2` | 5424160 | `37558262fe31587616f0ef047226e6fe86e1d09dfd550acb811f51f92d8974c6` |
| `app/src/fonts/NotoSansJP-VF.woff2` | 4350080 | `6fd94964d1990baa2a392cea7a036683d2b80b9dfd590cb3918e8dab98d8e188` |
| `app/src/fonts/NotoSansSC-VF.woff2` | 7782072 | `aef8c34277afad81ecd0227138a830263c0caea65b7aea66d1195395f097b55a` |
| `app/src/fonts/NotoSansKR-VF.woff2` | 3908716 | `f484190a87503ac2af2cf9771ec35122128453fe0cd92f4368117562d99547e6` |
