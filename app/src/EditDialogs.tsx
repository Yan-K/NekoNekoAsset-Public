import { useEffect, useLayoutEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode } from 'react';
import type { Locale, Product } from './model';
import { categoryKeys, sourceCategoryLabel, normalizeSearch, getEffectiveTags, getAvatarAliases, isLightTheme } from './model';
import { editorText } from './editor-i18n';
import { nativeWorkflowText } from './native-workflow-i18n';
import TagPicker, { resolveTag } from './TagPicker';
import AvatarPicker,{productsAvatarOptions,productAvatarOptions,productAvatarSelection,productAvatarValues,productAvatarDelta} from './AvatarPicker';
import Preview from './Preview';
import Toggle from './Toggle';
import {dialog, nativeFileAction, previewSource, errorCode, dismissAiSourceUrlCandidate} from './backend';
import type {AvatarOption,TagOption,FetchedProductMetadata,ProductOverlayChange} from './backend';
import {collectionMembership} from './collections';
import ConfirmDelete from './ConfirmDelete';
import {Pencil,Trash2,Check,X,Plus,Users} from 'lucide-react';
import {getCurrentWebview} from '@tauri-apps/api/webview';
import {isTauri} from '@tauri-apps/api/core';
import {localImportText} from './local-import-i18n';
import './UnityWorkflow.css';
import './editor-styles.css';

export interface EditDialogsProps {
  dialog: string | null; locale: Locale; products: Product[]; selected: string[]; active: Product | undefined;
  aiFeaturesEnabled?:boolean;
  onClose: () => void; onPatch: (ids: string[], patch: Partial<Product>) => void | Promise<void>;
  onBatchPatch?: (changes: ProductOverlayChange[]) => void | Promise<void>;
  onNotice: (s: string) => void; onSaveSearch: (name: string) => void | Promise<void>; onCollection: (name: string, ids: string[], included: boolean) => void | Promise<void>;
  collections?: Record<string, string[]>;
  collectionIds?: Record<string,string>;
  collectionBusy?: boolean;
  savedSearches?: {id?:string;name:string}[];
  tagOptions?:TagOption[];
  avatarOptions?:AvatarOption[];
  avatarLabel?:(id:string)=>string;
  onRefreshAvatars?:()=>void|Promise<void>;
  onFetchMetadata?:(url:string)=>Promise<FetchedProductMetadata>;
  onLocalFiles?:(productId:string,args:{draftId:string;addSources:string[];removeFileIds:string[]})=>Promise<unknown>;
  onCreateCollection?: (name:string)=>void | Promise<void>;
  onRenameCollection?: (oldName:string,newName:string)=>void | Promise<void>;
  onDeleteCollection?: (name:string,capturedId?:string)=>void | Promise<void>;
  onRenameSearch?: (index:number,name:string)=>void | Promise<void>;
  onDeleteSearch?: (index:number,capturedId?:string)=>void | Promise<void>;
  themeTokens: Record<string, string>; onThemePreview: (tokens: Record<string, string> | null) => void;
  onThemeSave: (tokens: Record<string, string>, name?: string) => void | Promise<void>; themeName?: string;
}
export const builtInTheme = { bg:'#171717', panel:'#202020', raised:'#292929', border:'#373737', text:'#ededed', muted:'#a1a1a1', accent:'#b6daca', 'secondary-accent':'#8bb8f0', 'secondary-accent-text':'#171717', control:'#303030', positive:'#b6daca', warning:'#dfc689', negative:'#e49c9c' };
const tokenLabels: Record<string, string> = { bg:'background', panel:'surface', raised:'raisedSurface', border:'border', text:'text', muted:'secondaryText', accent:'accent', 'secondary-accent':'secondaryAccent', 'secondary-accent-text':'secondaryAccentText', control:'controls', positive:'success', warning:'attention', negative:'failed' };
const prefixed = (tokens: Record<string,string>) => Object.fromEntries(Object.entries(tokens).map(([key,value]) => ['--'+key.replace(/^--/, ''),value]));
const secondaryThemeDefaults = (tokens:Record<string,string>) => isLightTheme(prefixed(tokens))
  ? {'secondary-accent':'#356cae','secondary-accent-text':'#ffffff'}
  : {'secondary-accent':'#8bb8f0','secondary-accent-text':'#171717'};
const validColor = (color: unknown): color is string => typeof color === 'string' && /^#[0-9a-fA-F]{6}$/.test(color);
export function parseThemeConfig(text: string): Record<string, string> {
  const value = JSON.parse(text);
  if (!value || value.format !== 'NekoNekoAsset.Theme' || value.version !== 1 || !value.tokens || typeof value.tokens !== 'object' || Array.isArray(value.tokens)) throw new Error('invalidTheme');
  if (Object.keys(value.tokens).some(key => !Object.hasOwn(builtInTheme, key))) throw new Error('invalidTheme');
  const optional = secondaryThemeDefaults(value.tokens);
  if (Object.keys(builtInTheme).some(key => (!Object.hasOwn(optional,key)||Object.hasOwn(value.tokens,key))&&!validColor(value.tokens[key]))) throw new Error('invalidTheme');
  return {...optional,...value.tokens};
}
export async function exportThemeConfig(tokens: Record<string,string>, name = ''): Promise<boolean> {
  const normalized = Object.fromEntries(Object.entries(tokens).map(([key,value])=>[key.replace(/^--/,''),value]));
  const config = {format:'NekoNekoAsset.Theme',version:1,name:name.trim(),tokens:normalized};
  config.tokens = parseThemeConfig(JSON.stringify(config));
  const chosen = await dialog('save_file',{defaultName:'NekoNekoAsset-theme.json',filters:[{name:'JSON',extensions:['json']}]});
  if (!chosen) return false;
  await nativeFileAction('write_text',{path:chosen.path,text:JSON.stringify(config,null,2)});
  return true;
}
const operationLabels:Record<Locale,string>={en:'Could not complete this action. Your changes are retained.',ja:'操作を完了できませんでした。変更は保持されています。',ko:'작업을 완료하지 못했습니다. 변경 사항은 유지됩니다.','zh-Hans':'无法完成操作。更改已保留。','zh-Hant':'無法完成操作。變更已保留。'};
const nativeLabels:Record<Locale,string>={en:'This action requires the desktop app.',ja:'この操作にはデスクトップアプリが必要です。',ko:'이 작업은 데스크톱 앱에서 사용할 수 있습니다.','zh-Hans':'此操作需要桌面应用。','zh-Hant':'此操作需要桌面應用程式。'};
const operationMessage=(locale:Locale,code:string)=>code==='nativeUnavailable'?nativeLabels[locale]:operationLabels[locale];
export function normalizeProductUrl(value:string):string|null {
  const trimmed=value.trim();if(!trimmed)return '';
  if(!/^https?:\/\//i.test(trimmed)||/[\u0000-\u0020\u007f]/.test(trimmed))return null;
  try{const url=new URL(trimmed);return url.hostname&&!url.username&&!url.password&&['http:','https:'].includes(url.protocol)?trimmed:null;}catch{return null;}
}
function Field({ label, children }: { label: string; children: ReactNode }) { return <label className="field"><span>{label}</span>{children}</label>; }
function equal(a: unknown, b: unknown) { return JSON.stringify(a) === JSON.stringify(b); }
const withPendingTag = (tags: string[], pending: string, suggestions: string[], aliases: Record<string, readonly string[]> = {}) => { const tag=resolveTag(pending,suggestions,aliases); return tag && !tags.some(value=>normalizeSearch(value)===normalizeSearch(tag)) ? [...tags,tag] : tags; };
const sameTag = (left:string,right:string) => { const aliases=new Set(getAvatarAliases(left).map(normalizeSearch)); return getAvatarAliases(right).some(alias=>aliases.has(normalizeSearch(alias))); };
const focusable = (element: HTMLDivElement | null) => Array.from(element?.querySelectorAll<HTMLElement>('button:not(:disabled),input:not(:disabled):not([type=hidden]),select:not(:disabled),textarea:not(:disabled),[tabindex="0"]') ?? []).filter(el => el.offsetParent !== null && !el.closest('[inert]'));
const localFileName = (path:string) => path.replace(/[\\/]+$/,'').split(/[\\/]/).pop() || path;

function DialogBody(props: EditDialogsProps) {
  const [target] = useState(()=>({active:props.active,selected:[...props.selected]}));
  const p = {...props,...target};
  const L = (key: string) => editorText(p.locale, key);
  const active = p.active;
  // This editor is keyed by product. Background source/AI updates must not
  // turn untouched opening values into manual overrides when another field saves.
  const opening = useRef(active).current;
  const offline=opening?.provider==='local';
  const batch = p.dialog === 'batch';
  const productMap=useMemo(()=>new Map(p.products.map(item=>[item.id,item])),[p.products]);
  const [openingBatch]=useState(()=>batch?p.selected.flatMap(id=>{const item=productMap.get(id);return item?[item]:[];}):[]);
  const scoped = batch?openingBatch.map(item=>productMap.get(item.id)??item):p.selected.flatMap(id=>{const item=productMap.get(id);return item?[item]:[];});
  const allSuggestions = useMemo(()=>[...new Set([...(p.tagOptions??[]).map(option=>option.label),...p.products.flatMap(product=>[...getEffectiveTags(product),...(product.tagSuggestions??[]).map(option=>option.label)])])],[p.products,p.tagOptions]);
  const [title, setTitle] = useState(active?.title ?? '');
  const [sourceUrl,setSourceUrl]=useState(active?.sourceUrl??'');
  const [ignoredAiUrls,setIgnoredAiUrls]=useState<string[]>([]);
  const AIText=(key:string)=>nativeWorkflowText(p.locale,key);
  const aiUrlCandidates=(p.aiFeaturesEnabled?active?.aiSourceUrlCandidates??[]:[]).filter(candidate=>!ignoredAiUrls.includes(candidate.url)&&normalizeProductUrl(candidate.url)!==null);
  const [resetTitle, setResetTitle] = useState(false);
  const [note, setNote] = useState(batch ? '' : active?.note ?? '');
  const [addedFiles,setAddedFiles]=useState<string[]>([]);
  const [removedFiles,setRemovedFiles]=useState<string[]>([]);
  const [filesDropping,setFilesDropping]=useState(false);
  const localFileList=useRef<HTMLDivElement>(null);
  const filesDraft=useRef(crypto.randomUUID());
  const localFilesEditable=p.dialog==='edit'&&active?.provider==='local'&&!!p.onLocalFiles;
  const appendLocalFiles=(paths:string[])=>setAddedFiles(current=>[...new Set([...current,...paths.filter(Boolean)])]);
  const chooseLocalFiles=async()=>{if(inFlight.current)return;try{const picked=await dialog('open_file',{multiple:true});if(picked&&alive.current&&!inFlight.current&&!confirmationOpen.current)appendLocalFiles(picked.paths??[picked.path]);}catch(error){if(alive.current)setError(errorCode(error));}};
  const returnToFileList=()=>requestAnimationFrame(()=>localFileList.current?.focus({preventScroll:true}));
  const [category, setCategory] = useState(batch ? '' : active?.category ?? 'NonVRChat');
  const [initialAvatarOptions]=useState(()=>batch?productsAvatarOptions(scoped,p.avatarOptions??[]):opening?productAvatarOptions(opening,p.avatarOptions??[]):p.avatarOptions??[]);
  const avatarOptions=useMemo(()=>[...(p.avatarOptions??[]),...initialAvatarOptions.filter(option=>'legacyValue' in option)],[p.avatarOptions,initialAvatarOptions]);
  const [initialAvatarSelection]=useState(()=>!batch&&opening?productAvatarSelection(opening,initialAvatarOptions):[]);
  const [selectedAvatars,setSelectedAvatars]=useState(initialAvatarSelection);
  const [initialBatchAvatarSelections]=useState(()=>Object.fromEntries(scoped.map(item=>[item.id,productAvatarSelection(item,initialAvatarOptions)])));
  const [batchAvatarSelections,setBatchAvatarSelections]=useState(initialBatchAvatarSelections);
  const batchAvatarEnabled=(id:string)=>scoped.length>0&&scoped.every(item=>batchAvatarSelections[item.id]?.includes(id));
  const batchAvatarMixed=(id:string)=>!batchAvatarEnabled(id)&&scoped.some(item=>batchAvatarSelections[item.id]?.includes(id));
  const pickerSelection=batch?avatarOptions.filter(option=>batchAvatarEnabled(option.id)).map(option=>option.id):selectedAvatars;
  const batchAvatarCount=avatarOptions.filter(option=>scoped.some(item=>batchAvatarSelections[item.id]?.includes(option.id))).length;
  const batchAvatarDeltas=scoped.map(item=>({id:item.id,delta:productAvatarDelta(avatarOptions,initialBatchAvatarSelections[item.id]??[],batchAvatarSelections[item.id]??[])}));
  const batchAvatarChanged=batchAvatarDeltas.some(item=>item.delta.add.length||item.delta.remove.length);
  const [avatarsRefreshing,setAvatarsRefreshing]=useState(false);
  const [avatarPickerOpen,setAvatarPickerOpen]=useState(false);
  const avatarPickerShown=useRef(avatarPickerOpen);avatarPickerShown.current=avatarPickerOpen;
  const avatarTrigger=useRef<HTMLButtonElement>(null),avatarWasOpen=useRef(false);
  useLayoutEffect(()=>{if(avatarWasOpen.current&&!avatarPickerOpen)avatarTrigger.current?.focus();avatarWasOpen.current=avatarPickerOpen;},[avatarPickerOpen]);
  const avatarChanged=!equal([...selectedAvatars].sort(),[...initialAvatarSelection].sort());
  const initialTags = useMemo(()=>opening ? getEffectiveTags(opening) : [],[opening]);
  const aliases = useMemo(()=>Object.fromEntries(allSuggestions.map(tag=>[tag,[...getAvatarAliases(tag),...(p.tagOptions??[]).find(option=>option.label===tag)?.aliases??[]]])),[allSuggestions,p.tagOptions]);
  const [tags, setTags] = useState(initialTags);
  const [pendingTag, setPendingTag] = useState('');
  const [tagMode, setTagMode] = useState('unchanged');
  const [tagDelta, setTagDelta] = useState<string[]>([]);
  const [pendingDelta, setPendingDelta] = useState('');
  const [rejected, setRejected] = useState(active?.rejectedTags ?? []);
  const suggestions = useMemo(()=>batch?allSuggestions:allSuggestions.filter(tag=>!rejected.some(value=>sameTag(value,tag))),[allSuggestions,rejected,batch]);
  const [sensitive, setSensitive] = useState(active?.sensitive ?? false);
  const initialSourcePreview=opening?.sourcePreviewUrl??(opening?.thumbnailPath?undefined:opening?.previewUrl);
  const [sourceThumbnailUrl,setSourceThumbnailUrl]=useState(active?.sourceThumbnailUrl);
  const sourcePreview=sourceThumbnailUrl??initialSourcePreview;
  const initialThumbnailChoice=opening?.thumbnailHidden?'none':opening?.thumbnailPath||opening?.thumbnailOverrideUrl?'custom':opening?.previewUrl?'source':'none';
  const initialThumbnailURL=previewSource(opening?.thumbnailOverrideUrl??(opening?.thumbnailPath?opening.previewUrl:undefined))??'';
  const [thumbnailChoice,setThumbnailChoice]=useState(initialThumbnailChoice);
  const [thumbnailURL,setThumbnailURL]=useState(initialThumbnailURL);
  const [thumbnailPath,setThumbnailPath]=useState<string|null>();

  const [rating,setRating]=useState('unchanged');
  const initialBatchRating=openingBatch.length>0&&openingBatch.every(item=>item.sensitive)?'on':openingBatch.some(item=>item.sensitive)?'mixed':'off';
  const batchRating=rating==='unchanged'?initialBatchRating:rating;
  const ratingChanged=rating!=='unchanged'&&rating!==initialBatchRating;
  const alive=useRef(true),thumbnailRequest=useRef(0),metadataRequest=useRef(0),metadataPending=useRef(false),draftUrl=useRef(sourceUrl);
  draftUrl.current=sourceUrl;
  const [fetching,setFetching]=useState<'metadata'|'thumbnail'|null>(null);
  const lastFetched=useRef<{url:string;thumbnailUrl:string|null}|null>(null);
  useEffect(()=>{alive.current=true;return()=>{alive.current=false;thumbnailRequest.current++;metadataRequest.current++;};},[]);
  const cancelFetch=()=>{metadataRequest.current++;metadataPending.current=false;setFetching(null);};
  const fetchDraft=async(thumbnailOnly=false)=>{
    if(inFlight.current||metadataPending.current||!p.onFetchMetadata)return;
    const url=normalizeProductUrl(sourceUrl);
    if(!url){setError('invalidURL');box.current?.querySelector<HTMLInputElement>('#editor-product-url')?.focus();return;}
    const request=++metadataRequest.current,thumbnailVersion=thumbnailRequest.current;
    metadataPending.current=true;
    setFetching(thumbnailOnly?'thumbnail':'metadata');setError('');
    try{
      const data=thumbnailOnly&&lastFetched.current?.url===url?lastFetched.current:await p.onFetchMetadata(url);
      if(!alive.current||request!==metadataRequest.current||normalizeProductUrl(draftUrl.current)!==url||inFlight.current)return;
      if(data.thumbnailUrl!==null&&!normalizeProductUrl(data.thumbnailUrl))throw new Error('invalidMetadata');
      if(!thumbnailOnly){
        const metadata=data as FetchedProductMetadata;
        const normalized=normalizeProductUrl(metadata.sourceUrl);
        if(!normalized||!metadata.title.trim()||!(categoryKeys as readonly string[]).includes(metadata.category)||!Array.isArray(metadata.tags)||metadata.tags.some(tag=>typeof tag!=='string'))throw new Error('invalidMetadata');
        setSourceUrl(normalized);draftUrl.current=normalized;setTitle(metadata.title);setResetTitle(false);setCategory(metadata.category);setTags([...metadata.tags]);setPendingTag('');setRejected(values=>values.filter(tag=>!metadata.tags.some(value=>sameTag(value,tag))));
      }
      const fetchedUrl=thumbnailOnly?url:(data as FetchedProductMetadata).sourceUrl.trim();
      lastFetched.current={url:fetchedUrl,thumbnailUrl:data.thumbnailUrl};
      setSourceThumbnailUrl(data.thumbnailUrl??'');
      if(thumbnailVersion===thumbnailRequest.current){setThumbnailPath(null);setThumbnailURL('');setThumbnailChoice(data.thumbnailUrl?'source':'none');}
    }catch(error){if(alive.current&&request===metadataRequest.current)setError(errorCode(error));}
    finally{if(alive.current&&request===metadataRequest.current){metadataPending.current=false;setFetching(null);}}
  };
  const resetThumbnail=()=>void fetchDraft(true);
  const ignoreAiUrl=async(url:string)=>{if(!active||inFlight.current||metadataPending.current)return;inFlight.current=true;setBusy(true);setError('');try{await dismissAiSourceUrlCandidate({productId:active.id,url});if(alive.current)setIgnoredAiUrls(values=>[...values,url]);}catch(error){if(alive.current)setError(errorCode(error));}finally{inFlight.current=false;if(alive.current)setBusy(false);}};
  const uploadThumbnail=async()=>{
    if(inFlight.current)return;
    cancelFetch();
    inFlight.current=true;setBusy(true);setError('');
    const request=++thumbnailRequest.current;
    try{
      const chosen=await dialog('open_file',{filters:[{name:'Images',extensions:['png','jpg','jpeg','webp']}]});
      if(!chosen||!alive.current||request!==thumbnailRequest.current)return;
      const copied=await nativeFileAction<{path:string;relativePath:string}>('copy_image',{path:chosen.path});
      if(!alive.current||request!==thumbnailRequest.current)return;
      setThumbnailPath(copied.path);setThumbnailURL(previewSource(copied.path)??'');setThumbnailChoice('custom');
    }catch(error){if(alive.current)setError(errorCode(error));}
    finally{inFlight.current=false;if(alive.current)setBusy(false);}
  };
  const [noteChanged, setNoteChanged] = useState(false);
  const [name, setName] = useState(p.dialog==='theme' ? p.themeName ?? '' : '');
  const [error, setError] = useState('');
  const [busy,setBusy]=useState(false);
  const inFlight=useRef(false);
  const [confirmClose, setConfirmClose] = useState(false);
  const [theme, setTheme] = useState<Record<string,string>>(() => ({ ...builtInTheme, ...secondaryThemeDefaults(p.themeTokens), ...Object.fromEntries(Object.entries(p.themeTokens).map(([key,value]) => [key.replace(/^--/, ''), value])) }));
  const initialTheme = useRef(theme);
  const lastValidTheme = useRef(theme);
  const box = useRef<HTMLDivElement>(null);
  const confirmation = useRef<HTMLDivElement>(null);
  const stayButton = useRef<HTMLButtonElement>(null);
  const saved = useRef(false);
  useEffect(()=>{if(!error)return;const frame=requestAnimationFrame(()=>box.current?.querySelector('[role="alert"]')?.scrollIntoView({block:'nearest'}));return()=>cancelAnimationFrame(frame);},[error]);
  const themeValid = Object.keys(builtInTheme).every(key => validColor(theme[key]));
  if(themeValid) lastValidTheme.current = theme;
  const singleChanged = opening && (resetTitle || title !== opening.title || sourceUrl!==(opening.sourceUrl??'') || sourceThumbnailUrl!==opening.sourceThumbnailUrl || note !== opening.note || category !== opening.category || avatarChanged || !equal(tags, initialTags) || !!pendingTag.trim() || !equal(rejected, opening.rejectedTags ?? []) || thumbnailPath!==undefined || thumbnailChoice !== initialThumbnailChoice || thumbnailURL !== initialThumbnailURL || sensitive !== opening.sensitive);
  const dirty = p.dialog === 'theme' ? !equal(theme, initialTheme.current) || name !== (p.themeName??'') : batch ? !!category || noteChanged || ratingChanged || batchAvatarChanged || (tagMode !== 'unchanged' && (!!tagDelta.length || !!pendingDelta.trim())) : p.dialog === 'edit' ? !!singleChanged||!!addedFiles.length||!!removedFiles.length : !!name.trim();
  const close = () => { if(inFlight.current)return;cancelFetch();if (p.dialog === 'theme') p.onThemePreview(null); p.onClose(); };
  const requestClose = () => {if(inFlight.current)return;cancelFetch();confirmClose ? setConfirmClose(false) : dirty ? setConfirmClose(true) : close();};
  const closeRef = useRef(requestClose); closeRef.current = requestClose;
  const confirmationOpen = useRef(confirmClose); confirmationOpen.current = confirmClose;
  useEffect(()=>{
    if(!localFilesEditable||!isTauri())return;
    let disposed=false;let unlisten:(()=>void)|undefined;
    void getCurrentWebview().onDragDropEvent(event=>{
      if(disposed)return;
      const payload=event.payload;
      if(payload.type==='leave'||inFlight.current||confirmationOpen.current||avatarPickerShown.current){setFilesDropping(false);return;}
      const target=document.elementFromPoint(payload.position.x/devicePixelRatio,payload.position.y/devicePixelRatio);
      const inside=!!target&&localFileList.current?.contains(target)===true;
      setFilesDropping(inside&&(payload.type==='enter'||payload.type==='over'));
      if(payload.type==='drop'&&inside)appendLocalFiles(payload.paths);
    }).then(stop=>{if(disposed)stop();else unlisten=stop;}).catch(()=>{});
    return()=>{disposed=true;unlisten?.();};
  },[localFilesEditable]);
  useEffect(() => {
    const prior = document.activeElement as HTMLElement | null;
    const elements=focusable(box.current); (elements.find(element=>element.tagName!=='BUTTON')??elements[0])?.focus();
    const key = (e: KeyboardEvent) => {
      if (e.isComposing || e.keyCode===229) return;
      if(avatarPickerShown.current)return;
      if (e.key === 'Escape') { e.stopPropagation(); e.preventDefault(); closeRef.current(); }
      if (e.key === 'Tab') {
        const elements = focusable(confirmationOpen.current ? confirmation.current : box.current);
        const first = elements[0], last = elements[elements.length - 1];
        if (e.shiftKey && (document.activeElement === first || !elements.includes(document.activeElement as HTMLElement))) { e.preventDefault(); last?.focus(); }
        else if (!e.shiftKey && (document.activeElement === last || !elements.includes(document.activeElement as HTMLElement))) { e.preventDefault(); first?.focus(); }
      }
    };
    document.addEventListener('keydown', key, true);
    return () => { document.removeEventListener('keydown', key, true); if(prior?.isConnected) prior.focus(); if (p.dialog === 'theme' && !saved.current) p.onThemePreview(null); };
  }, []);
  useEffect(() => {
    if(!confirmClose) return;
    const prior = document.activeElement as HTMLElement | null;
    stayButton.current?.focus();
    return ()=>{if(prior?.isConnected) prior.focus();};
  },[confirmClose]);
  useEffect(() => { if (p.dialog === 'theme' && themeValid) p.onThemePreview(prefixed(theme)); }, [theme]);
  const openAvatarPicker=async()=>{if(inFlight.current||avatarsRefreshing)return;setAvatarsRefreshing(true);setError('');try{await p.onRefreshAvatars?.();if(alive.current&&!inFlight.current&&!confirmationOpen.current)setAvatarPickerOpen(true);}catch(error){if(alive.current)setError(errorCode(error));}finally{if(alive.current)setAvatarsRefreshing(false);}};
  const invalid = (message:string) => { setConfirmClose(false); setError(message); requestAnimationFrame(()=>box.current?.querySelector<HTMLInputElement>('input[required]')?.focus()); };
  const save = async () => {
    if(inFlight.current)return;
    cancelFetch();
    inFlight.current=true;setBusy(true);setError('');
    try {
    if (p.dialog === 'edit' && active) {
      if (!title.trim()) { invalid('nameRequired'); return; }
      const finalUrl=normalizeProductUrl(sourceUrl);
      if(finalUrl===null){setConfirmClose(false);setError('invalidURL');requestAnimationFrame(()=>box.current?.querySelector<HTMLInputElement>('#editor-product-url')?.focus());return;}
      const finalTags=withPendingTag(tags,pendingTag,suggestions,aliases);
      const finalRejected=pendingTag.trim()?rejected.filter(tag=>!sameTag(tag,resolveTag(pendingTag,allSuggestions,aliases))):rejected;
      const patch: Partial<Product> = { custom: true, sourceTitle: active.sourceTitle ?? active.title };
      if (title !== opening?.title || resetTitle) { patch.title = title.trim(); patch.titleOverride = !resetTitle; }
      if(finalUrl!==(opening?.sourceUrl??''))patch.sourceUrl=finalUrl;
      if(sourceThumbnailUrl!==opening?.sourceThumbnailUrl)patch.sourceThumbnailUrl=sourceThumbnailUrl;
      if (note !== opening?.note) patch.note = note;
      if (category !== opening?.category) { patch.category = category; patch.categoryOverride = true; }
      if(avatarChanged&&opening)patch.ownedAvatars=productAvatarValues(opening,avatarOptions,selectedAvatars,initialAvatarOptions);
      if(sensitive !== opening?.sensitive) patch.sensitive=sensitive;
      if (!equal(finalTags,initialTags)) { patch.tags = finalTags; patch.sourceTags = active.sourceTags ?? [...active.tags]; }
      if (!equal(finalRejected,opening?.rejectedTags ?? [])) patch.rejectedTags = finalRejected;
      if(thumbnailPath!==undefined||thumbnailChoice!==initialThumbnailChoice){
        if(thumbnailChoice==='custom'){if(thumbnailPath)patch.thumbnailSourcePath=thumbnailPath;else patch.thumbnailHidden=false;}
        else if(thumbnailChoice==='source'||thumbnailPath===null)patch.thumbnailSourcePath=null;
        else {patch.thumbnailOverrideUrl='';patch.previewUrl='';}
      }
      await p.onPatch([active.id], patch);
      if(localFilesEditable&&(addedFiles.length||removedFiles.length))await p.onLocalFiles!(active.id,{draftId:filesDraft.current,addSources:addedFiles,removeFileIds:removedFiles});
    } else if (batch) {
      const delta=withPendingTag(tagDelta,pendingDelta,suggestions,aliases);
      const changes = scoped.map(item => {
        const patch: Partial<Product> = {};
        if (category) { patch.category = category; patch.categoryOverride=true; }
        if (noteChanged) patch.note = note;
        if(ratingChanged)patch.sensitive=rating==='on';
        if (tagMode === 'addTag' && delta.length) { patch.tags = [...new Set([...getEffectiveTags(item), ...delta])]; patch.rejectedTags=(item.rejectedTags??[]).filter(tag=>!delta.some(value=>sameTag(value,tag))); }
        if (tagMode === 'removeTag' && delta.length) { patch.tags = getEffectiveTags(item).filter(tag => !delta.some(value=>sameTag(value,tag))); patch.rejectedTags=[...new Set([...(item.rejectedTags??[]),...item.tags.filter(tag=>delta.some(value=>sameTag(value,tag)))])]; }
        if(patch.tags) patch.sourceTags=item.sourceTags??[...item.tags];
        const avatarDelta=batchAvatarDeltas.find(change=>change.id===item.id)?.delta;
        return { id: item.id, patch, ...(avatarDelta&&(avatarDelta.add.length||avatarDelta.remove.length)?{ownedAvatarsDelta:avatarDelta}:{}) };
      }).filter(change => Object.keys(change.patch).length||change.ownedAvatarsDelta);
      if (changes.length) { if (p.onBatchPatch) await p.onBatchPatch(changes); else for(const change of changes)await p.onPatch([change.id],change.patch); }
    } else if (p.dialog === 'theme') {
      if (!themeValid) { invalid('invalidTheme'); return; }
      if (!name.trim()) { invalid('themeNameRequired'); return; }
      await p.onThemeSave(prefixed(theme),name.trim()); saved.current = true; p.onThemePreview(null);
    } else if(p.dialog==='saveSearch') {
      if(!name.trim()) { invalid('nameRequired'); return; }
      await p.onSaveSearch(name.trim());
    }
    p.onClose();
    }catch(error){if(alive.current){setConfirmClose(false);setError(errorCode(error));}}
    finally{inFlight.current=false;if(alive.current)setBusy(false);}
  };
  const updateTags=(next:string[])=>{
    const added=next.filter(tag=>!tags.includes(tag)); const removed=tags.filter(tag=>!next.includes(tag));
    setTags(next.filter(tag=>!removed.some(value=>sameTag(value,tag))));
    setRejected(values=>[...new Set([...values.filter(tag=>!added.some(value=>sameTag(value,tag))),...removed])]);
  };
  const importTheme = async () => {
    if(inFlight.current)return;
    inFlight.current=true;setBusy(true);setError('');
    try {
      const chosen=await dialog('open_file',{filters:[{name:'JSON',extensions:['json']}]});
      if(!chosen)return;
      const result=await nativeFileAction('read_text',{path:chosen.path});
      const text=typeof result==='string'?result:result.text??result.content;
      if(typeof text!=='string'||new TextEncoder().encode(text).byteLength>32_768)throw new Error('invalidTheme');
      const tokens=parseThemeConfig(text); const imported=JSON.parse(text);
      if(!alive.current)return;
      setTheme(tokens); if(typeof imported.name==='string'&&imported.name.trim()) setName(imported.name.trim()); setError('');
    } catch(error) { if(alive.current)setError(error instanceof SyntaxError?'invalidTheme':errorCode(error)); }
    finally{inFlight.current=false;if(alive.current)setBusy(false);}
  };
  const exportTheme = async () => {
    if(inFlight.current)return;
    if (!themeValid) { setError('invalidTheme'); return; }
    inFlight.current=true;setBusy(true);setError('');
    try{await exportThemeConfig(theme,name);}
    catch(error){if(alive.current)setError(errorCode(error));}
    finally{inFlight.current=false;if(alive.current)setBusy(false);}
  };
  const thumbnailPreview=thumbnailChoice==='custom'?thumbnailURL:thumbnailChoice==='source'?previewSource(sourcePreview):undefined;
  const themeDialogStyle = p.dialog === 'theme' ? { ...prefixed(lastValidTheme.current), color:lastValidTheme.current.text, colorScheme:isLightTheme(prefixed(lastValidTheme.current))?'light':'dark' } as CSSProperties : undefined;
  const labelKey = p.dialog === 'saveSearch' ? 'saveSearch' : p.dialog ?? 'edit';
  return <>
    <div className="modal-backdrop" onMouseDown={e => {if(e.target === e.currentTarget) requestClose();}}>
      <div className="modal editor-modal" role="dialog" aria-modal={!confirmClose&&!avatarPickerOpen} aria-hidden={confirmClose||avatarPickerOpen||undefined} inert={confirmClose||avatarPickerOpen} aria-busy={busy} aria-labelledby="edit-dialog-title" ref={box} style={themeDialogStyle}>
        <div className="modal-header"><h2 id="edit-dialog-title">{L(labelKey)}{batch && ` · ${scoped.length}`}</h2><button disabled={busy} aria-label={L('close')} onClick={requestClose}>×</button></div>
        <form onSubmit={e=>{e.preventDefault();save();}} style={{display:'contents'}}>
          <div className="modal-body" inert={busy}>
            {(p.dialog === 'edit' || batch) && <>
              {!batch && <><Field label={L('title')}><input value={title} onChange={e=>{setTitle(e.target.value);setResetTitle(false);}} required /></Field>{!offline&&<div className="form-row"><small className="muted">{L('sourceValue')}: {active?.sourceTitle ?? active?.title}</small><button type="button" onClick={()=>{setTitle(active?.sourceTitle ?? active?.title ?? '');setResetTitle(true);}}>{L('resetToSource')}</button></div>}</>}
              {!batch&&<Field label={L('productURL')}><div className="editor-url-control"><input id="editor-product-url" inputMode="url" autoCapitalize="none" spellCheck={false} value={sourceUrl} aria-invalid={normalizeProductUrl(sourceUrl)===null} onChange={e=>{draftUrl.current=e.target.value;setSourceUrl(e.target.value);cancelFetch();if(error==='invalidURL')setError('');}}/><button type="button" disabled={!!fetching||!p.onFetchMetadata} aria-busy={fetching==='metadata'} onClick={()=>void fetchDraft()}>{L(fetching==='metadata'?'fetchingProductData':'fetchProductData')}</button></div></Field>}
              {!batch&&aiUrlCandidates.map(candidate=><div className="field" key={candidate.url}><small>{AIText('aiUrlCandidate')} · {AIText(candidate.status==='supported'?'aiUrlSupported':'aiUrlUncertain')}</small><small style={{overflowWrap:'anywhere'}}>{candidate.url}</small><small title={candidate.evidenceRefs.join(' · ')}>{AIText(candidate.basis)}</small><div className="form-row"><button type="button" disabled={!!fetching} onClick={()=>{const url=normalizeProductUrl(candidate.url);if(!url)return;cancelFetch();draftUrl.current=url;setSourceUrl(url);if(error==='invalidURL')setError('');}}>{AIText('aiUseUrl')}</button><button type="button" disabled={!!fetching} onClick={()=>void ignoreAiUrl(candidate.url)}>{AIText('aiIgnoreUrl')}</button></div></div>)}
              <div className="editor-category-row"><Field label={L('category')}><select value={category} onChange={e=>setCategory(e.target.value)}>{batch && <option value="">{L('unchanged')} · {new Set(scoped.map(p=>p.category)).size > 1 ? L('mixed') : L(scoped[0]?.category ?? 'unknown')}</option>}{categoryKeys.map(key=><option key={key} value={key}>{L(key)}</option>)}</select></Field><button type="button" className="editor-avatar-trigger" ref={avatarTrigger} title={L('compatibleAvatars')} aria-label={L('compatibleAvatars')} aria-haspopup="dialog" aria-expanded={avatarPickerOpen} aria-busy={avatarsRefreshing} disabled={avatarsRefreshing||(batch&&!p.onBatchPatch)} onClick={()=>void openAvatarPicker()}><Users size={14}/><span>{L('editAvatars')}</span>{(batch?batchAvatarCount:selectedAvatars.length)>0&&<small>{batch?batchAvatarCount:selectedAvatars.length}</small>}</button></div>
              {!batch&&!offline&&active?.sourceCategory&&<small className="muted">{[...new Set((active.sources??[]).map(source=>source.provider).filter(Boolean))].map(provider=>provider+' · ').join('')}{L('sourceValue')}: {sourceCategoryLabel(active.sourceCategory)||L('unknown')}</small>}
              {batch ? <><Field label={L('tags')}><select value={tagMode} onChange={e=>{setTagMode(e.target.value);setTagDelta([]);setPendingDelta('');}}>{['unchanged','addTag','removeTag'].map(key=><option key={key} value={key}>{L(key)}</option>)}</select></Field>{tagMode !== 'unchanged' && <TagPicker key={tagMode} locale={p.locale} label={L(tagMode)} value={tagDelta} suggestions={tagMode==='removeTag'?[...new Set(scoped.flatMap(item=>getEffectiveTags(item)))]:suggestions} aliases={aliases} onChange={setTagDelta} onDraftChange={setPendingDelta}/>}</> : <TagPicker locale={p.locale} value={tags} suggestions={suggestions} aliases={aliases} onChange={updateTags} onDraftChange={setPendingTag}/>}
              {batch && <label className="form-row"><input type="checkbox" checked={noteChanged} onChange={e=>setNoteChanged(e.target.checked)}/>{L('replaceNote')}</label>}
              {batch&&<Field label={L('note')}><textarea rows={3} value={note} disabled={!noteChanged} placeholder={L('unchanged')} onChange={e=>setNote(e.target.value)}/></Field>}
              {batch&&<div className="editor-batch-rating"><span className="editor-sensitive-toggle"><span className="nna-toggle"><span id="editor-batch-sensitive-label">R-18</span><button type="button" role="checkbox" aria-checked={batchRating==='mixed'?'mixed':batchRating==='on'} aria-labelledby="editor-batch-sensitive-label" title={ratingChanged?L(batchRating==='on'?'ratingOn':'ratingOff'):L('unchanged')+(batchRating==='mixed'?' · '+L('mixed'):'')} onClick={()=>setRating(batchRating==='on'?'off':'on')}><span aria-hidden="true"/></button></span></span></div>}
              {!batch&&<>
                <div className="editor-thumbnail-note"><div><div className="field"><span>{L('thumbnail')}</span></div><div className="editor-thumbnail-preview"><Preview src={thumbnailPreview} title={L('thumbnail')} placeholderTitle={L('noImage')}/></div></div><Field label={L('note')}><textarea rows={5} value={note} onChange={e=>setNote(e.target.value)}/></Field></div>
                <div className="editor-thumbnail-bottom"><div className="editor-thumbnail-actions"><button type="button" disabled={!!fetching||!p.onFetchMetadata} aria-busy={fetching==='thumbnail'} onClick={resetThumbnail}>{L(fetching==='thumbnail'?'resettingThumbnail':'resetThumbnail')}</button><button type="button" onClick={()=>void uploadThumbnail()}>{L('uploadThumbnail')}</button></div><label className="editor-sensitive-toggle" htmlFor="editor-sensitive"><Toggle id="editor-sensitive" label="R-18" checked={sensitive} onChange={setSensitive}/></label></div>
                {localFilesEditable&&<div className="editor-local-files"><div className="unity-file-heading editor-local-file-heading"><strong>{L('files')}</strong><button type="button" onClick={()=>void chooseLocalFiles()}>{L('addFiles')}</button></div><div className={`unity-file-list editor-local-file-list${filesDropping?' is-dropping':''}`} ref={localFileList} tabIndex={0} role="group" aria-label={L('files')} title={localImportText(p.locale,'dropFiles')} onDragOver={event=>{if(event.dataTransfer.types.includes('Files')){event.preventDefault();setFilesDropping(true);}}} onDragLeave={event=>{if(!(event.relatedTarget instanceof Node)||!event.currentTarget.contains(event.relatedTarget))setFilesDropping(false);}} onDrop={event=>{event.preventDefault();setFilesDropping(false);}}>{active?.files.filter(file=>!removedFiles.includes(file.id)).map(file=><div className="unity-file-row editor-local-file-row" key={file.id}><span className="unity-file-name"><span title={file.name}>{file.name}</span></span><button type="button" title={L('removeFile')} aria-label={`${L('removeFile')} ${file.name}`} onClick={()=>{setRemovedFiles(ids=>[...new Set([...ids,file.id])]);returnToFileList();}}><X size={14}/></button></div>)}{addedFiles.map(path=><div className="unity-file-row editor-local-file-row" key={path}><span className="unity-file-name"><span title={path}>{localFileName(path)}</span></span><button type="button" title={L('removeFile')} aria-label={`${L('removeFile')} ${localFileName(path)}`} onClick={()=>{setAddedFiles(paths=>paths.filter(value=>value!==path));returnToFileList();}}><X size={14}/></button></div>)}{!addedFiles.length&&!active?.files.some(file=>!removedFiles.includes(file.id))&&<div className="editor-local-file-empty">{localImportText(p.locale,'dropFiles')}</div>}</div></div>}
              </>}
            </>}
            {p.dialog === 'saveSearch' && <Field label={L('name')}><input value={name} required onChange={e=>setName(e.target.value)}/></Field>}
            {p.dialog === 'theme' && <>
              <Field label={L('themeName')}><input value={name} required onChange={e=>setName(e.target.value)}/></Field>
              <div className="form-row"><button type="button" onClick={()=>{setTheme({...builtInTheme});setError('');}}>{L('reset')}</button><button type="button" onClick={()=>void importTheme()}>{L('importTheme')}</button><button type="button" onClick={()=>void exportTheme()}>{L('exportTheme')}</button></div>
              {Object.keys(builtInTheme).map(key=><div className="form-row" key={key}><label htmlFor={`theme-${key}`}>{L(tokenLabels[key])}</label><div className="color-field"><input type="color" aria-label={`${L(tokenLabels[key])} ${L('preview')}`} value={validColor(theme[key]) ? theme[key] : '#000000'} onChange={e=>setTheme(v=>({...v,[key]:e.target.value}))}/><input id={`theme-${key}`} type="text" value={theme[key]} aria-invalid={!validColor(theme[key])} onChange={e=>setTheme(v=>({...v,[key]:e.target.value}))}/></div></div>)}
              <div style={{...Object.fromEntries(Object.entries(theme).filter(([,v])=>validColor(v)).map(([k,v])=>[`--${k}`,v])),background:'var(--bg)',color:'var(--text)',border:'1px solid var(--border)',borderRadius:5,padding:14} as CSSProperties}>
                <strong>{L('preview')}</strong><div className="form-row"><button type="button">{L('edit')}</button><button type="button" className="primary">{L('selected')}</button><button type="button" style={{background:'var(--secondary-accent)',borderColor:'var(--secondary-accent)',color:'var(--secondary-accent-text)'}}>{L('unity')}</button><button type="button" disabled>{L('disabled')}</button></div><div className="form-row"><span className="badge ready">{L('ready')}</span><span className="badge warning">{L('attention')}</span><span className="badge error">{L('failed')}</span></div>
              </div>
            </>}
            {error && <div className="callout error" role="alert">{['nameRequired','invalidURL','invalidTheme','invalidThumbnail','themeNameRequired'].includes(error)?L(error):operationMessage(p.locale,error)}</div>}
          </div>
          <div className="modal-footer"><button type="button" disabled={busy} onClick={close}>{L('cancel')}</button><button className="primary" type="submit" disabled={busy || (batch && (!scoped.length || !dirty)) || (p.dialog === 'theme' && !themeValid) || (p.dialog === 'edit' && !active)}>{L('save')}</button></div>
        </form>
      </div>
    </div>
    {avatarPickerOpen&&<AvatarPicker locale={p.locale} options={avatarOptions} value={pickerSelection} mixed={batch?batchAvatarMixed:undefined} label={id=>{const option=avatarOptions.find(option=>option.id===id);return option&&'legacyValue' in option?String(option.legacyValue):p.avatarLabel?.(id)||option?.displayName||option?.officialName||id;}} onChange={(ids,selected)=>{if(batch)setBatchAvatarSelections(current=>Object.fromEntries(scoped.map(item=>{const values=current[item.id]??[];return [item.id,selected?[...new Set([...values,...ids])]:values.filter(id=>!ids.includes(id))];})));else setSelectedAvatars(current=>selected?[...new Set([...current,...ids])]:current.filter(id=>!ids.includes(id)));}} onClose={()=>setAvatarPickerOpen(false)}/>}
    {confirmClose && <div className="modal-backdrop editor-confirm-backdrop" onMouseDown={e=>{if(e.target===e.currentTarget&&!inFlight.current)setConfirmClose(false);}}>
      <div className="modal editor-confirm" role="alertdialog" aria-modal="true" aria-labelledby="editor-unsaved-title" ref={confirmation} style={themeDialogStyle}>
        <div className="modal-header"><h2 id="editor-unsaved-title">{L('unsavedChanges')}</h2></div>
        <div className="modal-footer"><button type="button" disabled={busy} onClick={()=>void save()}>{L('save')}</button><button type="button" disabled={busy} onClick={close}>{L('discard')}</button><button type="button" disabled={busy} className="primary" ref={stayButton} onClick={()=>setConfirmClose(false)}>{L('stay')}</button></div>
      </div>
    </div>}
  </>;
}
function CollectionDialog(p:EditDialogsProps){
 const L=(key:string)=>editorText(p.locale,key);
 // Removing members from the active filter may change the library selection.
 // Keep this popup's targets fixed until it closes.
 const [ids]=useState(()=>[...new Set(p.selected)]);
 const [products]=useState(()=>{const selected=new Set(ids);return p.products.filter(product=>selected.has(product.id));});
 const [name,setName]=useState(''),[error,setError]=useState(''),[creating,setCreating]=useState(false);
 const inFlight=useRef(false),alive=useRef(true),box=useRef<HTMLDivElement>(null),close=useRef(p.onClose);close.current=p.onClose;
 useEffect(()=>{alive.current=true;return()=>{alive.current=false;};},[]);
 useEffect(()=>{if(!error)return;const frame=requestAnimationFrame(()=>box.current?.querySelector('[role="alert"]')?.scrollIntoView({block:'nearest'}));return()=>cancelAnimationFrame(frame);},[error]);
 useEffect(()=>{const prior=document.activeElement as HTMLElement|null;box.current?.querySelector<HTMLInputElement>('input')?.focus();const key=(e:KeyboardEvent)=>{if(e.isComposing||e.keyCode===229)return;if(e.key==='Escape'){e.preventDefault();e.stopPropagation();close.current();}if(e.key==='Tab'){const elements=focusable(box.current),first=elements[0],last=elements[elements.length-1];if(e.shiftKey&&(document.activeElement===first||!elements.includes(document.activeElement as HTMLElement))){e.preventDefault();last?.focus();}else if(!e.shiftKey&&(document.activeElement===last||!elements.includes(document.activeElement as HTMLElement))){e.preventDefault();first?.focus();}}};document.addEventListener('keydown',key,true);return()=>{document.removeEventListener('keydown',key,true);if(prior?.isConnected)prior.focus();};},[]);
 const create=async()=>{
  if(inFlight.current||!p.onCreateCollection)return;
  const trimmed=name.trim();
  if(!trimmed){setError('nameRequired');return;}
  if(Object.keys(p.collections??{}).some(value=>normalizeSearch(value)===normalizeSearch(trimmed))){setError('collectionExists');return;}
  inFlight.current=true;setCreating(true);setError('');
  try{await p.onCreateCollection(trimmed);if(alive.current){setName('');requestAnimationFrame(()=>{if(document.activeElement===document.body||box.current?.querySelector('form')?.contains(document.activeElement))box.current?.querySelector<HTMLInputElement>('input')?.focus();});}}
  catch(error){if(alive.current)setError(errorCode(error));}
  finally{inFlight.current=false;if(alive.current)setCreating(false);}
 };
 const change=async(name:string,included:boolean)=>{setError('');try{await p.onCollection(name,ids,included);}catch(error){if(alive.current)setError(errorCode(error));}};
 return <div className="modal-backdrop" onMouseDown={e=>{if(e.target===e.currentTarget)p.onClose();}}><div className="modal editor-modal editor-collection-modal" role="dialog" aria-modal="true" aria-busy={p.collectionBusy||creating} aria-labelledby="editor-collection-title" ref={box}>
  <header className="modal-header"><h2 id="editor-collection-title">{L('manageCollections')}</h2><button type="button" aria-label={L('close')} onClick={p.onClose}>×</button></header>
  <div className="modal-body">
   <form onSubmit={e=>{e.preventDefault();void create();}}><label className="field" htmlFor="editor-new-collection"><span>{L('newCollection')}</span></label><div className="editor-collection-create"><input id="editor-new-collection" disabled={creating} value={name} onChange={e=>{setName(e.target.value);setError('');}} onKeyDown={e=>{if(e.key==='Enter'&&(e.nativeEvent.isComposing||e.keyCode===229))e.preventDefault();}}/><button type="submit" className="primary" title={L('createCollection')} aria-label={L('createCollection')} aria-busy={creating} disabled={creating||!name.trim()||!p.onCreateCollection}><Plus size={18}/></button></div></form>
   {Object.keys(p.collections??{}).length>0&&<><div className="editor-collection-heading" id="editor-collections-label">{L('existingCollections')}</div><div className="editor-collections" role="group" aria-labelledby="editor-collections-label">{Object.entries(p.collections??{}).map(([name,members])=>{const state=collectionMembership(members,ids);return <label key={name}><input type="checkbox" checked={state.checked} aria-checked={state.mixed?'mixed':state.checked} ref={input=>{if(input)input.indeterminate=state.mixed;}} disabled={!ids.length} title={state.mixed?L('mixed'):undefined} onChange={()=>void change(name,!state.checked)}/><span>{name}</span><small className="muted">{members.length}</small></label>;})}</div></>}
   <div className="editor-selected-products"><div className="data-row" id="editor-selected-products-label"><span>{L('selected')}</span><span>{ids.length}</span></div><ul tabIndex={0} aria-labelledby="editor-selected-products-label">{products.map(product=><li key={product.id}>{product.title}</li>)}</ul></div>
   {error&&<div className="callout error" role="alert">{['nameRequired','collectionExists'].includes(error)?L(error):L('collectionActionFailed')}</div>}
  </div><footer className="modal-footer"><button type="button" onClick={p.onClose}>{L('close')}</button></footer>
 </div></div>;
}
function ManagerDialog(p:EditDialogsProps){
 const L=(key:string)=>editorText(p.locale,key),collections=p.dialog==='manageCollections';
 const names=collections?Object.keys(p.collections??{}):(p.savedSearches??[]).map(search=>search.name);
 const [removeTarget,setRemoveTarget]=useState<{id?:string;name:string;index:number}|null>(null);
 const confirming=useRef(false);confirming.current=!!removeTarget;
 const [name,setName]=useState(''),[editing,setEditing]=useState<number|null>(null),[renamed,setRenamed]=useState(''),[error,setError]=useState(''),[busy,setBusy]=useState(false),[pendingAction,setPendingAction]=useState<'add'|'rename'|'remove'|null>(null);
 const inFlight=useRef(false),alive=useRef(true);
 useEffect(()=>{alive.current=true;return()=>{alive.current=false;};},[]);
 const pending=!collections&&busy;
 const requestClose=()=>{if(!confirming.current&&(collections||!inFlight.current))p.onClose();};
 const box=useRef<HTMLDivElement>(null),close=useRef(requestClose);close.current=requestClose;
 useEffect(()=>{if(!error)return;const frame=requestAnimationFrame(()=>box.current?.querySelector('[role="alert"]')?.scrollIntoView({block:'nearest'}));return()=>cancelAnimationFrame(frame);},[error]);
 useEffect(()=>{const prior=document.activeElement as HTMLElement|null;box.current?.querySelector<HTMLInputElement>('input')?.focus();const key=(e:KeyboardEvent)=>{if(e.isComposing||e.keyCode===229||confirming.current)return;if(e.key==='Escape'){e.preventDefault();e.stopPropagation();close.current();}if(e.key==='Tab'){const elements=focusable(box.current),first=elements[0],last=elements[elements.length-1];if(e.shiftKey&&(document.activeElement===first||!elements.includes(document.activeElement as HTMLElement))){e.preventDefault();last?.focus();}else if(!e.shiftKey&&(document.activeElement===last||!elements.includes(document.activeElement as HTMLElement))){e.preventDefault();first?.focus();}}};document.addEventListener('keydown',key,true);return()=>{document.removeEventListener('keydown',key,true);if(prior?.isConnected)prior.focus();};},[]);
 const validate=(value:string,skip=-1)=>{if(!value.trim()){setError('nameRequired');return false;}if(names.some((other,index)=>index!==skip&&normalizeSearch(other)===normalizeSearch(value))){setError('nameExists');return false;}setError('');return true;};
 const run=async(action:()=>void|Promise<void>,done:()=>void,pending:'add'|'rename'|'remove')=>{if(collections){setError('');try{const result=action();done();void Promise.resolve(result).catch(error=>{if(alive.current)setError(errorCode(error));});}catch(error){setError(errorCode(error));}return;}if(inFlight.current)return;inFlight.current=true;setBusy(true);setPendingAction(pending);setError('');try{await action();if(alive.current)done();}catch(error){if(alive.current)setError(errorCode(error));}finally{inFlight.current=false;if(alive.current){setBusy(false);setPendingAction(null);}}};
 const add=async()=>{if(!validate(name)||collections&&!p.onCreateCollection)return;await run(()=>collections?p.onCreateCollection?.(name.trim()):p.onSaveSearch(name.trim()),()=>setName(''),'add');};
 const rename=async(index:number)=>{if(!validate(renamed,index))return;await run(()=>collections?p.onRenameCollection?.(names[index],renamed.trim()):p.onRenameSearch?.(index,renamed.trim()),()=>setEditing(null),'rename');};
 const remove=async()=>{if(!removeTarget?.id)throw new Error('entryUnavailable');await (collections?p.onDeleteCollection?.(removeTarget.name,removeTarget.id):p.onDeleteSearch?.(removeTarget.index,removeTarget.id));if(alive.current)setEditing(null);};
 return <><div className="modal-backdrop" onMouseDown={e=>{if(e.target===e.currentTarget)requestClose();}}><div className="modal editor-modal editor-manager" role="dialog" aria-modal={!removeTarget} aria-hidden={!!removeTarget||undefined} inert={!!removeTarget} aria-busy={pending} aria-labelledby="editor-manager-title" ref={box}>
  <header className="modal-header"><h2 id="editor-manager-title">{L(collections?'manageCollections':'manageSearches')}</h2><button type="button" disabled={busy&&!collections} aria-label={L('close')} onClick={requestClose}>×</button></header>
  <div className="modal-body" inert={pending}><form className="editor-manager-add" onSubmit={e=>{e.preventDefault();void add();}}><input disabled={pending} aria-label={L('name')} placeholder={L('name')} value={name} onChange={e=>{setName(e.target.value);setError('');}}/><button type="submit" disabled={pending||collections&&!p.onCreateCollection}>{L(busy&&pendingAction==='add'?(collections?'creatingEntry':'addingEntry'):'addEntry')}</button></form>
   <div className="editor-manager-list">{names.map((value,index)=><div className="editor-manager-row" key={collections?p.collectionIds?.[value]??value:p.savedSearches?.[index]?.id??index}>{editing===index?<><input aria-label={L('rename')+' '+value} autoFocus value={renamed} onChange={e=>setRenamed(e.target.value)} onKeyDown={e=>{if(e.nativeEvent.isComposing||e.keyCode===229)return;if(e.key==='Enter'){e.preventDefault();rename(index);}}}/><button type="button" aria-label={L('save')} title={L('save')} onClick={()=>rename(index)}><Check size={14}/></button><button type="button" aria-label={L('cancel')} title={L('cancel')} onClick={()=>{setEditing(null);setError('');}}><X size={14}/></button></>:<><span title={value}>{value}</span><button type="button" aria-label={L('rename')+' '+value} title={L('rename')} disabled={collections?!p.onRenameCollection:!p.onRenameSearch} onClick={()=>{setEditing(index);setRenamed(value);setError('');}}><Pencil size={14}/></button><button type="button" aria-label={L('delete')+' '+value} title={L('delete')} disabled={collections?!p.onDeleteCollection:!p.onDeleteSearch} onClick={()=>setRemoveTarget({id:collections?p.collectionIds?.[value]:p.savedSearches?.[index]?.id,name:value,index})}><Trash2 size={14}/></button></>}</div>)}</div>{error&&<div className="callout error" role="alert">{["nameRequired","nameExists"].includes(error)?L(error):operationMessage(p.locale,error)}</div>}
  </div><footer className="modal-footer"><button type="button" disabled={busy&&!collections} onClick={requestClose}>{L('close')}</button></footer>
 </div></div>{removeTarget&&<ConfirmDelete locale={p.locale} kind={collections?'collection':'search'} name={removeTarget.name} onDelete={remove} onClose={()=>setRemoveTarget(null)}/>}</>;
}
export function EditDialogs(props:EditDialogsProps){if(props.dialog==='collection')return <CollectionDialog {...props}/>;if(props.dialog==='manageCollections'||props.dialog==='manageSearches')return <ManagerDialog key={props.dialog} {...props}/>;if(!props.dialog||!['edit','batch','saveSearch','theme'].includes(props.dialog))return null;return <DialogBody key={props.dialog} {...props}/>;}
export default EditDialogs;
