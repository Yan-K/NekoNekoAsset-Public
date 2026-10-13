import type {Ref} from 'react';
type ArtProps = { kind?: number; title?: string; small?: boolean; sensitive?: boolean;elementRef?:Ref<SVGSVGElement> };

/** Missing-preview glyph only. Real preview images are supplied by the catalogue. */
export default function Art({ title, small = false, sensitive = false,elementRef }: ArtProps) {
  return <svg ref={elementRef} className={`asset-art missing-art${small ? ' small' : ''}${sensitive ? ' sensitive' : ''}`} viewBox="0 0 240 220" role="img" aria-label={title}>
    <rect width="240" height="220" fill="var(--raised)"/>
    <g fill="none" stroke="var(--muted)" strokeWidth="1.5" opacity=".55">
      <rect x="92" y="83" width="56" height="48" rx="5"/>
      <circle cx="132" cy="98" r="5"/>
      <path d="M94 123L109 108L122 120L128 114L145 130M92 142H148"/>
    </g>
  </svg>;
}
