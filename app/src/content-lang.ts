// Product text is mostly Japanese, but the UI may be Chinese or Korean. Han characters share code points across
// those languages, so Japanese text needs lang="ja" to get Japanese glyph forms and the Japanese font stack
// (calm-studio.css). Kana is the only reliable signal; Han-only text keeps the UI language.
const kana=/[぀-ヿㇰ-ㇿｦ-ﾟ]/;
export const contentLang=(text?:string|null):'ja'|undefined=>text&&kana.test(text)?'ja':undefined;
