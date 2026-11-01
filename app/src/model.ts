import {t} from './i18n';
export type Locale = 'en' | 'ja' | 'ko' | 'zh-Hans' | 'zh-Hant';
export interface AcquisitionProvenance { rowId: string; source: string; acquisitionKey: string | null; acquiredDateText: string | null; acquisitionKind: string | null; variantName: string | null; accountId?: string }
export interface FileProvenance { rowId: string; source: string; acquisitionKey: string | null; variantId: string | null; variantName: string | null; accountId?: string }
export interface AssetFile { id: string; name: string; type: string; size: string; status: string; provenance?: FileProvenance[]; sizeBytes?:number; rootId?:string; relativePath?:string; originalAvailable?:boolean; verifiedHash?:string; errorCode?:string }
export interface SourceCategory { id: string | null; name: string | null; parentId: string | null; parentName: string | null }
export interface Account { id: string; name: string; store: string; status: string; lastSync?:string;lastSyncAt?:string;authenticatedAt?:string }
export interface AvatarEvidence { avatar: string; source: 'accountVariant' | 'accountFilename' | 'user' | 'sourceTag' | 'userTag'; accountId?: string; fileId?: string;avatarId?:string;entitlementEstablished?:false }
export interface CategoryInference { category: string; basis: 'sourceCategory' | 'tags' | 'fallback' }
export interface Product {
  id: string; title: string; shop: string; category: string; tags: string[]; provider?:string; hidden?:boolean;
  avatar: string; listingAvatars: string[]; ownedAvatars: string[]; files: AssetFile[];
  status: string; downloaded: number; total: number; price: number | null; filesLoaded?:boolean; fullyDownloaded?:boolean; unavailable?:boolean;
  priceCurrency?:string|null; currentPriceText?:string|null; listingStartingPrice?:number|null; paidPrice?:number|null; paidCurrency?:string|null;
  knownFileCount?:number;
  ownedCurrentPrices?:{variantId:string;name:string;price:number|null;currency:string|null;accountIds:string[]}[];
  ownedPaidPrices?:{variantId:string;name:string;price:number|null;currency:string|null;accountId?:string;acquisitionKey?:string;quantity?:number}[];
  acquired: string; note: string; sensitive: boolean; kind?: number;
  accountIds?: string[]; categoryOverride?: boolean; categoryInference?: CategoryInference; ownedAvatarEvidence?: AvatarEvidence[]; sourceTags?: string[];
  importSource?: { kind: 'local' | 'remote'; location: string; files: string[]; productUrl?: string; thumbnailName?: string };
  thumbnailOverrideUrl?: string; sourcePreviewUrl?: string; thumbnailSourcePath?:string|null; thumbnailPath?:string; thumbnailHidden?:boolean;
  newlyDiscovered?: boolean; sourceCategory?: SourceCategory; custom?: boolean; sourceTitle?: string; titleOverride?: boolean; manifestComplete?: boolean; rejectedTags?: string[]; sourceKind?: number; previewUrl?: string; provenance?: AcquisitionProvenance[];
  tagSuggestions?:{id:string;label:string;kind:string;basis:string;inferred:boolean}[];
  sourceUrl?:string;sourceUrlAvailable?:boolean;publicPageAvailability?:'available'|'unavailable'|'unknown';
  sourceOriginUrl?:string;sourceUrlOverride?:boolean;
  sourceThumbnailUrl?:string;
  aiSourceUrlCandidates?:{url:string;basis:string;confidence:number;evidenceRefs:string[];status:'supported'|'uncertain'}[];
  sources?:{provider:string;accountId?:string;accountName?:string;label:string}[];
}
/** Native item-line evidence supplies paid amount/currency; listing price never supplies paid facts. */
export function priceLabel(locale:Locale,value:number|null|undefined,currency:string|null|undefined):string|null {
 if(typeof value!=='number'||!Number.isFinite(value)||value<0||!currency)return null;
 if(currency==='JPY')return Number.isSafeInteger(value)?`¥${value.toLocaleString(locale)}`:null;
 if(!Intl.supportedValuesOf('currency').includes(currency))return null;
 return new Intl.NumberFormat(locale,{style:'currency',currency}).format(value);
}
function startingPriceLabel(locale:Locale,product:Pick<Product,'provider'|'listingStartingPrice'>):string|null {
 if(product.provider!=='booth')return null;
 const amount=priceLabel(locale,product.listingStartingPrice,'JPY');
 return amount?`${t(locale,'startingPrice')} ${amount}`:null;
}
/** An owned quote takes priority; BOOTH's starting price is explicitly labelled when no owned quote is available. */
export function currentPriceLabel(locale:Locale,product:Pick<Product,'provider'|'price'|'priceCurrency'|'currentPriceText'|'ownedCurrentPrices'|'listingStartingPrice'>):string|null {
 if(product.provider==='booth'){
  const rows=product.ownedCurrentPrices??[];
  if(rows.some(row=>priceLabel(locale,row.price,row.currency)))return rows.map(row=>[row.name,priceLabel(locale,row.price,row.currency)||'—'].filter(Boolean).join(' · ')).join(' / ');
  return startingPriceLabel(locale,product);
 }
 if(product.ownedCurrentPrices?.length)return product.ownedCurrentPrices.map(row=>[row.name,priceLabel(locale,row.price,row.currency)||'—'].filter(Boolean).join(' · ')).join(' / ');
 return priceLabel(locale,product.price,product.priceCurrency)||product.currentPriceText?.trim()||null;
}
/** Compact cards omit variant names while keeping starting prices distinct from owned quotes. */
export function cardPriceLabel(locale:Locale,product:Pick<Product,'provider'|'price'|'priceCurrency'|'ownedCurrentPrices'|'listingStartingPrice'>):string|null {
 const rows=product.ownedCurrentPrices?.length?product.ownedCurrentPrices:[{price:product.price,currency:product.priceCurrency}];
 const labels=rows.map(row=>{
  if(!priceLabel(locale,row.price,row.currency))return null;
  return new Intl.NumberFormat(locale,{style:'currency',currency:row.currency!,minimumFractionDigits:0}).format(row.price!);
 });
 return labels.some(Boolean)?[...new Set(labels.map(label=>label||'—'))].join(' / '):startingPriceLabel(locale,product);
}
/** Known descriptors remain visible while unverified membership has an explicit +. */
export function fileCountLabel(product:Pick<Product,'manifestComplete'|'downloaded'|'total'|'knownFileCount'|'files'>,ratio=true):string {
 const count=product.knownFileCount??product.files.length;
 if(product.manifestComplete===true)return ratio?`${product.downloaded}/${product.total}`:String(product.total);
 if(!count)return '—';
 return `${ratio?product.downloaded+'/':''}${count}+`;
}
/** All assets is the intended VRChat default; explicit shortcuts retain their scopes. */
export function libraryCategoryMatches(product: Product, category: string, quick = 'all', collection = ''): boolean {
  if (category !== 'all') return product.category === category;
  return !!collection || quick !== 'all' || product.category !== 'NonVRChat';
}
export function isFileDownloaded(file: AssetFile): boolean {
  return ['ready', 'downloaded', 'extracting', 'extractionFailed'].includes(file.status);
}
export type SortKey = 'acquired' | 'title' | 'price' | 'status';
export type SortDirection = 'asc' | 'desc';
export function defaultSortDirection(key: string): SortDirection { return key === 'acquired' ? 'desc' : 'asc'; }
/** Unknown values stay last before direction is applied to known facts. */
export function compareProducts(a: Product, b: Product, key: string, direction: SortDirection, locale: Locale): number {
  const value = (product: Product): string | number | null => {
    if (key === 'price') return product.price !== null && Number.isFinite(product.price) ? product.price : null;
    if (key === 'status') { const status = getProductDisplayStatus(product); return status === 'unknown' ? null : status; }
    return (key === 'title' ? product.title : product.acquired).trim() || null;
  };
  const left = value(a), right = value(b);
  if (left === null || right === null) return left === right ? 0 : left === null ? 1 : -1;
  const order = typeof left === 'number' && typeof right === 'number' ? left-right : String(left).localeCompare(String(right),locale);
  return direction === 'desc' ? -order : order;
}
export const thumbnailSizes = [0,96,112,128,160,192,224,272,320] as const;
/** The fitted width drives both rendered cards and virtual row geometry. */
export function resultGeometry(width: number, size: number, density: number, metadataCount?:number) {
  const available = Math.max(1,width-24), list = size === 0;
  const gap = density === 40 ? 8 : density === 74 ? 13 : 16;
  const columns = list ? 1 : Math.max(1,Math.floor((available+gap)/(size+gap)));
  const actual = list ? 0 : (available-gap*(columns-1))/columns;
  const info = metadataCount===undefined ? (density === 40 ? 88 : density === 74 ? 96 : 110) : 74+Math.max(0,metadataCount)*18;
  return {list,gap,columns,actual,info,row:list?density:actual/1.15+info+gap};
}
/** Checked collections are additive; unchecked memberships remain intact. */
export function addToCollections(collections: Record<string,string[]>, names: readonly string[], ids: readonly string[]): Record<string,string[]> {
  const additions = [...new Set(names)].filter(name => name.trim()).map(name => [name,[...new Set([...(Object.hasOwn(collections,name)?collections[name]:[]),...ids])]]);
  return {...collections,...Object.fromEntries(additions)};
}
/** Native controls follow the actual selected/previewed background, including imported themes. */
export function isLightTheme(tokens: Record<string,string>): boolean {
  const hex=tokens['--bg']; if(!/^#[a-f0-9]{6}$/i.test(hex??'')) return false;
  const channels=[1,3,5].map(offset=>{const value=parseInt(hex.slice(offset,offset+2),16)/255;return value<=0.04045?value/12.92:((value+0.055)/1.055)**2.4;});
  return channels[0]*0.2126+channels[1]*0.7152+channels[2]*0.0722>0.45;
}
export const categoryKeys = ['Base', 'Clothes', 'Hair', 'Accessory', 'Texture', 'Motion', 'Tool', 'Shader', 'Gimmick', 'World', 'Udon', 'NonVRChat'] as const;
/** User-defined categories; only explicit edits assign their `custom:` IDs. */
export interface CustomCategory { id: string; name: string }
export function categoryName(category: string, custom: readonly CustomCategory[], label: (key: string) => string): string {
  return category.startsWith('custom:') ? custom.find(entry => entry.id === category)?.name ?? label('unknown') : label(category);
}
/** Opaque source keys never assert a normalized asset type or avatar compatibility. */
export function sourceCategoryKey(category: SourceCategory | undefined): string | null {
  return category?.name?.trim() ? `booth-category:${JSON.stringify([category.id, category.parentId, category.parentName, category.name])}` : null;
}
export function sourceCategoryLabel(category: SourceCategory): string {
  return [category.parentName, category.name].filter(value => value?.trim()).join(' / ');
}
export function categoryChoices(_products: readonly Product[] = []): { key: string; sourceLabel?: string }[] {
  return categoryKeys.map(key => ({ key }));
}
// Original best-fit rules. Specific component evidence outranks a broad avatar/world parent.
const categoryRules: { category: typeof categoryKeys[number]; pattern: RegExp; specific: boolean }[] = [
  { category: 'Udon', pattern: /\budon(?:sharp)?\b|udonシステム|udon系统|udon系統/i, specific: true },
  { category: 'Shader', pattern: /\bshaders?\b|シェーダー?|셰이더|着色器|著色器/i, specific: true },
  { category: 'Hair', pattern: /\bhair(?:styles?)?\b|髪型|ヘア|헤어|发型|髮型/i, specific: true },
  { category: 'Clothes', pattern: /\b(?:cloth(?:es|ing)?|outfits?|costumes?|coats?|dresses?)\b|衣装|衣服|服装|服裝|의상/i, specific: true },
  { category: 'Accessory', pattern: /\b(?:accessor(?:y|ies)|props?|brooch(?:es)?|jewel(?:ry|lery))\b|アクセサリー|小物|配饰|配飾|액세서리/i, specific: true },
  { category: 'Texture', pattern: /\b(?:textures?|materials?)\b|テクスチャ|マテリアル|素材|贴图|貼圖|材质|材質|텍스처|머티리얼/i, specific: true },
  { category: 'Motion', pattern: /\b(?:motions?|animations?|gestures?|poses?)\b|モーション|アニメーション|动作|動作|모션|애니메이션/i, specific: true },
  { category: 'Tool', pattern: /\b(?:tools?|utilities|utility|editor extension)\b|ツール|工具|도구/i, specific: true },
  { category: 'Gimmick', pattern: /\b(?:gimmicks?|interactions?)\b|ギミック|交互组件|互動元件|기믹/i, specific: true },
  { category: 'World', pattern: /\b(?:worlds?|environments?)\b|ワールド|世界|월드/i, specific: false },
  { category: 'Base', pattern: /\b(?:avatar(?: base)?|base avatar|3d characters?)\b|アバター|素体|素體|아바타/i, specific: false },
  { category: 'NonVRChat', pattern: /\b(?:non[ -]?vrchat|brush(?:es)?|print(?:ing)?|illustrations?)\b|VRChat以外|イラスト|笔刷|筆刷/i, specific: true },
];
export function inferCategoryEvidence(sourceCategory: SourceCategory | string | undefined, tags: readonly string[] = []): CategoryInference {
  const leaf = typeof sourceCategory === 'string' ? sourceCategory : sourceCategory?.name ?? '';
  const parent = typeof sourceCategory === 'string' ? '' : sourceCategory?.parentName ?? '';
  let winner: CategoryInference = { category: 'NonVRChat', basis: 'fallback' }; let best = 0;
  for (const rule of categoryRules) {
    const leafMatch = rule.pattern.test(normalizeSearch(leaf));
    const tagMatch = tags.some(tag => rule.pattern.test(normalizeSearch(tag)));
    const parentMatch = rule.pattern.test(normalizeSearch(parent));
    const score = (leafMatch ? 6 : tagMatch ? 4 : parentMatch ? 2 : 0) + ((leafMatch || tagMatch || parentMatch) && rule.specific ? 5 : 0);
    if (score > best) { best = score; winner = { category: rule.category, basis: leafMatch || parentMatch && !tagMatch ? 'sourceCategory' : 'tags' }; }
  }
  return winner;
}
/** Inferred classification never establishes VRChat compatibility or avatar ownership. */
export function inferCategory(sourceCategory: SourceCategory | string | undefined, tags: readonly string[] = []): typeof categoryKeys[number] {
  return inferCategoryEvidence(sourceCategory, tags).category as typeof categoryKeys[number];
}
export const statusKeys = ['notDownloaded', 'partial', 'processing', 'ready', 'attention'] as const;
export type ProductDisplayStatus = typeof statusKeys[number] | 'unknown';
export type SensitiveMode = 'show' | 'blur' | 'hide' | 'only';
export function sensitiveMatches(product: Product, mode: SensitiveMode): boolean {
  return mode === 'only' ? product.sensitive === true : mode === 'hide' ? product.sensitive !== true : true;
}
export function accountMatches(product: Product, ids: readonly string[] | null): boolean {
  return ids === null || ids.some(id => product.accountIds?.includes(id));
}
/** Product badges are derived from file integrity, without replacing detailed file states. */
export function getProductDisplayStatus(product: Product): ProductDisplayStatus {
  if(product.filesLoaded===false)return statusKeys.includes(product.status as typeof statusKeys[number])?product.status as ProductDisplayStatus:product.downloaded>0?'partial':'notDownloaded';
  // Native projections decide actionable failures; a remaining byte ratio is not an error.
  if(typeof product.fullyDownloaded==='boolean'&&product.status!=='ready')return statusKeys.includes(product.status as typeof statusKeys[number])?product.status as ProductDisplayStatus:product.downloaded>0?'partial':'notDownloaded';
  const summary = summarizeFiles(product.files, product.manifestComplete);
  const states = [product.status, ...product.files.map(file => file.status)];
  if (states.some(state => ['queued', 'processing', 'downloading', 'transferring', 'extracting'].includes(state))) return 'processing';
  if (states.some(state => ['failed', 'extractionFailed', 'missing'].includes(state))) return 'attention';
  if (summary.usable) return 'ready';
  if (product.status==='attention') return 'attention';
  if (summary.downloaded > 0) return 'partial';
  return 'notDownloaded';
}
export function getEffectiveAvatarTags(product: Product, accountIds: readonly string[] | null = null): string[] {
  if (product.provider === 'local') accountIds = null;
  if (accountIds?.length === 0) return [];
  return [...new Set(product.ownedAvatars)].filter(avatar => {
    if (accountIds === null) return true;
    const evidence = (product.ownedAvatarEvidence ?? []).filter(item => item.avatar === avatar);
    // An explicit compatibility correction has no account restriction. Raw account evidence does.
    return !evidence.length || evidence.some(item => ['user','sourceTag','userTag'].includes(item.source) || item.accountId !== undefined && accountIds.includes(item.accountId));
  });
}
// Original, small alias table from human-provided examples and authored fixtures.
// This resolves exact tag spellings; it does not guess product identity or ownership.
const avatarAliasGroups: Record<string, readonly string[]> = {
  '真冬 / Mafuyu': ['真冬', 'まふゆ', 'Mafuyu', 'Mahuyu', '真冬用', '真冬對應', '真冬对应'],
  'マヌカ / Manuka': ['マヌカ', 'Manuka', 'マヌカ用', 'マヌカ対応'],
  'まめひなた / Mamehinata': ['まめひなた', 'Mamehinata', 'まめひなた用', 'まめひなた対応'],
  '紙羽 / Kamihane': ['紙羽', 'Kamihane'],
  'こいし / Koishi Study': ['こいし', 'Koishi Study'],
  'ルーム / Loom': ['ルーム', 'Loom'],
  'しずく / Shizuku Study': ['しずく', 'Shizuku Study'],
};
const avatarAliasIndex = new Map(Object.entries(avatarAliasGroups).flatMap(([canonical, aliases]) => [canonical, ...aliases].map(alias => [normalizeSearch(alias), canonical] as const)));
export function canonicalizeTag(tag: string): string { return avatarAliasIndex.get(normalizeSearch(tag)) ?? tag.normalize('NFKC').trim(); }
export function getAvatarAliases(avatar: string): string[] {
  const canonical = avatarAliasIndex.get(normalizeSearch(avatar));
  return [...new Set([avatar, ...(canonical ? [canonical, ...avatarAliasGroups[canonical]] : avatar.split(/\s*\/\s*/))].map(value => value.trim()).filter(Boolean))];
}
const avatarAliases = (avatar: string): string[] => getAvatarAliases(avatar).map(normalizeSearch);
/** Listing-only avatar tags stay raw evidence, and do not become an owned-content filter. */
export function getEffectiveTags(product: Product): string[] {
  const offered = new Set(product.listingAvatars.flatMap(avatarAliases));
  const owned = new Set(getEffectiveAvatarTags(product).flatMap(avatarAliases));
  const rejected = new Set((product.rejectedTags ?? []).flatMap(avatarAliases));
  return product.tags.filter(tag => !rejected.has(normalizeSearch(tag)) && (!offered.has(normalizeSearch(tag)) || owned.has(normalizeSearch(tag))));
}
export interface AppState {
  products: Product[]; selectedIds: string[]; locale: Locale;
  query: string; category: string; status: string;
  theme: 'dark' | 'light' | 'custom'; density: number; thumbnail: number;
  autoDownloadAll: boolean; collections: Record<string, string[]>;
  savedSearches: { id: string; name: string; query: string; category: string; status: string }[];
}
export interface FileSummary { downloaded: number; total: number; status: string; usable: boolean }
/** This shortcut checks complete byte evidence, independently of extraction readiness. */
export function isProductDownloaded(product: Product): boolean {
  if(product.filesLoaded===false)return product.fullyDownloaded===true;
  return product.files.length > 0 && product.files.every(isFileDownloaded);
}
/** Browser object URLs and their reset source exist only for the current session. */
export function stripEphemeralThumbnail(product: Product): Product {
  const {thumbnailOverrideUrl: _override, sourcePreviewUrl: _source, ...safe} = product;
  if (safe.previewUrl?.startsWith('blob:')) delete safe.previewUrl;
  return safe;
}
/** Bytes present is distinct from validated, usable output. Unknown is never ready. */
export function summarizeFiles(files: readonly AssetFile[], _manifestComplete = false): FileSummary {
  const total = files.length;
  const downloaded = files.filter(f => ['ready', 'downloaded', 'extracting', 'extractionFailed'].includes(f.status)).length;
  let status = 'notDownloaded';
  if (!total || files.some(f => f.status === 'unknown')) status = 'unknown';
  else if (files.every(f => f.status === 'ready')) status = 'ready';
  else if (files.some(f => f.status === 'extractionFailed')) status = 'attention';
  else if (files.some(f => f.status === 'extracting')) status = 'extracting';
  else if (files.some(f => f.status === 'failed' || f.status === 'missing')) status = 'attention';
  else if (downloaded > 0 && downloaded < total) status = 'partial';
  else if (files.some(f => f.status === 'paused')) status = 'paused';
  else if (files.some(f => f.status === 'queued')) status = 'queued';
  else if (downloaded === total) status = 'extracting';
  return { downloaded, total, status, usable: status === 'ready' };
}
export function normalizeSearch(value: string): string { return value.normalize('NFKC').toLocaleLowerCase().trim(); }
export function productMatches(product: Product, query: string): boolean {
  const text = normalizeSearch([product.id, product.title, product.sourceTitle, product.shop, product.note, ...getEffectiveTags(product), ...getEffectiveAvatarTags(product), ...product.files.map(f => f.name)].filter(Boolean).join(' '));
  return normalizeSearch(query).split(/\s+/).every(term => text.includes(term));
}
export type EditableField = 'title' | 'category' | 'tags' | 'note' | 'ownedAvatars' | 'sensitive' | 'rejectedTags';
export type ProductPatch = Partial<Pick<Product, EditableField>>;
export interface UndoEntry { id: string; before: ProductPatch; after: ProductPatch; wasCustom?: boolean; beforeTitleOverride?: boolean; afterTitleOverride?: boolean; beforeCategoryOverride?: boolean; afterCategoryOverride?: boolean }
export interface UndoChange { entries: UndoEntry[] }
/** Only explicitly edited fields participate, so undo cannot erase unrelated later edits. */
export function applyProductPatch(products: readonly Product[], ids: readonly string[], patch: ProductPatch): { products: Product[]; undo: UndoChange } {
  const scope = new Set(ids); const entries: UndoEntry[] = [];
  const result = products.map(product => {
    if (!scope.has(product.id)) return product;
    const before: ProductPatch = {};
    for (const field of Object.keys(patch) as EditableField[]) Object.assign(before, { [field]: structuredClone(product[field]) });
    entries.push({ id: product.id, before, after: structuredClone(patch), wasCustom: product.custom, beforeTitleOverride: product.titleOverride, afterTitleOverride: Object.hasOwn(patch, 'title') ? true : product.titleOverride, beforeCategoryOverride: product.categoryOverride, afterCategoryOverride: Object.hasOwn(patch, 'category') ? true : product.categoryOverride });
    return { ...product, ...structuredClone(patch), custom: true, titleOverride: Object.hasOwn(patch, 'title') ? true : product.titleOverride, categoryOverride: Object.hasOwn(patch, 'category') ? true : product.categoryOverride, sourceTitle: product.sourceTitle ?? product.title, sourceTags: product.sourceTags ?? [...product.tags] };
  });
  return { products: result, undo: { entries } };
}
export function undoProductPatch(products: readonly Product[], change: UndoChange): { products: Product[]; conflicts: string[] } {
  const entries = new Map(change.entries.map(entry => [entry.id, entry])); const conflicts: string[] = [];
  const result = products.map(product => {
    const entry = entries.get(product.id); if (!entry) return product;
    const fields = Object.keys(entry.after) as EditableField[];
    if (fields.some(field => JSON.stringify(product[field]) !== JSON.stringify(entry.after[field])) || (Object.hasOwn(entry.after, 'title') && product.titleOverride !== entry.afterTitleOverride) || (Object.hasOwn(entry.after, 'category') && product.categoryOverride !== entry.afterCategoryOverride)) { conflicts.push(product.id); return product; }
    return { ...product, ...structuredClone(entry.before), custom: entry.wasCustom, titleOverride: Object.hasOwn(entry.before, 'title') ? entry.beforeTitleOverride : product.titleOverride, categoryOverride: Object.hasOwn(entry.before, 'category') ? entry.beforeCategoryOverride : product.categoryOverride };
  });
  return { products: result, conflicts };
}
/** Mock source refresh retains the user layer and never infers identity from names. */
export function refreshSource(product: Product, source: Partial<Pick<Product, 'title' | 'shop' | 'listingAvatars' | 'sourceCategory' | 'tags'>>): Product {
  const rawCategory = source.sourceCategory ?? product.sourceCategory;
  const inference = inferCategoryEvidence(rawCategory, source.tags ?? product.tags);
  const categoryChanged = Object.hasOwn(source, 'sourceCategory') || Object.hasOwn(source, 'tags');
  return { ...product, shop: source.shop ?? product.shop, listingAvatars: source.listingAvatars ?? product.listingAvatars, sourceCategory: rawCategory, category: product.categoryOverride === true || !categoryChanged ? product.category : inference.category, categoryInference: categoryChanged ? inference : product.categoryInference, sourceTags: source.tags ?? product.sourceTags ?? [...product.tags], sourceTitle: source.title ?? product.sourceTitle ?? product.title, title: product.titleOverride === true ? product.title : source.title ?? product.title };
}

/** Clears only the title override. Notes, tags, classification and other corrections remain intact. */
export function resetSourceTitle(product: Product): Product { return { ...product, title: product.sourceTitle ?? product.title, titleOverride: false }; }


