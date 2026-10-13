import {useEffect,useMemo,useRef,useState,type ReactNode} from 'react';
import type {Props} from './Workflows';
import {categoryKeys,normalizeSearch} from './model';
import {t} from './i18n';
import {errorCode,nativeFileAction,previewSource} from './backend';
import TagPicker,{resolveTag} from './TagPicker';
import {localImportText} from './local-import-i18n';
import {getCurrentWebview} from '@tauri-apps/api/webview';
import {isTauri} from '@tauri-apps/api/core';
import './local-import.css';
import Preview from './Preview';

type LocalImportProps=Pick<Props,'locale'|'products'|'importSeed'|'libraryReady'|'nativeState'|'onNative'|'onLibraryReady'|'onClose'|'onNotice'|'onNewAssets'>&{
 onDirtyChange?:(dirty:boolean)=>void;
};
type Source={path:string;kind:'file'|'folder'|'unknown'};
type Draft={name:string;category:string;categoryManual:boolean;tags:string[];pendingTag:string;sourceUrl:string;thumbnail:{relativePath:string;path:string;name:string}|null;thumbnailUrl:string};
const emptyDraft=(name=''):Draft=>({name,category:'NonVRChat',categoryManual:false,tags:[],pendingTag:'',sourceUrl:'',thumbnail:null,thumbnailUrl:''});
const absolutePath=(path:string)=>/^(?:[A-Za-z]:[\\/]|\\\\[^\\]+\\[^\\]+|\/)/.test(path);
const fileName=(path:string)=>path.replace(/[\\/]+$/,'').split(/[\\/]/).pop()??path;
// Dialogs can return one selection, many selections, or the native wrapper object.
function selectedPaths(result:unknown):string[] {
 if(typeof result==='string')return result?[result]:[];
 if(Array.isArray(result))return result.flatMap(selectedPaths);
 if(result&&typeof result==='object'){
  const selected=result as {paths?:unknown;path?:unknown};
  return selectedPaths(selected.paths??selected.path);
 }
 return [];
}
function Field({label,children}:{label:string;children:ReactNode}){return <label className="field"><span>{label}</span>{children}</label>;}

export default function LocalImport(c:LocalImportProps){
 const L=(key:string)=>localImportText(c.locale,key);
 const seed=c.importSeed as (Props['importSeed']&{sources?:string[]});
 // Legacy seed.files are display names, not permission to read filesystem paths.
 const [sources,setSources]=useState<Source[]>(()=>[...new Set(seed?.sources??[])].filter(absolutePath).map(path=>({path,kind:'unknown'})));
 const [mode,setMode]=useState<'single'|'batch'>('single');
 const [singleDraft,setSingleDraft]=useState<Draft>(()=>emptyDraft(seed?.name??''));
 const [batchDrafts,setBatchDrafts]=useState<Record<string,Draft>>({});
 const [selectedPath,setSelectedPath]=useState('');
 const editorPath=sources.some(source=>source.path===selectedPath)?selectedPath:sources[0]?.path??'';
 const draft=mode==='single'?singleDraft:batchDrafts[editorPath]??emptyDraft(fileName(editorPath).replace(/\.zip$/i,''));
 const {name,category,categoryManual,tags,pendingTag,sourceUrl,thumbnail,thumbnailUrl}=draft;
 const updateDraft=(patch:Partial<Draft>)=>{if(mode==='single')setSingleDraft(current=>({...current,...patch}));else setBatchDrafts(current=>({...current,[editorPath]:{...(current[editorPath]??draft),...patch}}));};
 const setName=(name:string)=>updateDraft({name});
 const setCategory=(category:string)=>updateDraft({category,categoryManual:true});
 const setTags=(tags:string[])=>updateDraft({tags});
 const setPendingTag=(pendingTag:string)=>updateDraft({pendingTag});
 const setSourceUrl=(sourceUrl:string)=>updateDraft({sourceUrl});
 const setThumbnail=(thumbnail:Draft['thumbnail'])=>updateDraft({thumbnail,thumbnailUrl:''});
 const [dropping,setDropping]=useState(false);
 const [rootId,setRootId]=useState('');
 const [libraryGate,setLibraryGate]=useState(false);
 const [libraryPath,setLibraryPath]=useState('');
 const [busy,setBusy]=useState(false);
 const [error,setError]=useState('');
 const lock=useRef(false);
 const draftId=useRef(crypto.randomUUID());
 const accepted=useRef(false);
 const mounted=useRef(true);
 useEffect(()=>{mounted.current=true;return()=>{mounted.current=false;};},[]);
 const roots=c.nativeState?.roots??[];
 const configuredRoot=c.nativeState?.preferences.libraryRootId;
 const defaultRootId=typeof configuredRoot==='string'?configuredRoot:roots.find(root=>Reflect.get(root,'active')!==false)?.id;
 const destinationRootId=rootId||defaultRootId;
 const destination=roots.find(root=>root.id===destinationRootId);
 const ready=!!destination&&typeof destination.path==='string'&&!!destination.path.trim()&&['active','online','ready','configured'].includes(destination.status);
 const suggestions=useMemo(()=>[...new Set([...(c.nativeState?.tagOptions??[]).map(tag=>tag.label),...c.products.flatMap(product=>product.tags)])],[c.nativeState?.tagOptions,c.products]);
 const aliases=useMemo(()=>Object.fromEntries((c.nativeState?.tagOptions??[]).map(tag=>[tag.label,tag.aliases])),[c.nativeState?.tagOptions]);
 let validUrl=!sourceUrl.trim();
 try{const url=new URL(sourceUrl.trim());validUrl=['https:','http:'].includes(url.protocol)&&!url.username&&!url.password;}catch{}
 const batchInvalid=mode==='batch'&&sources.some(source=>source.kind==='file'&&!/\.zip$/i.test(source.path));
 const draftValid=(item:Draft)=>{if(!item.name.trim())return false;if(!item.sourceUrl.trim())return true;try{const url=new URL(item.sourceUrl.trim());return ['https:','http:'].includes(url.protocol)&&!url.username&&!url.password;}catch{return false;}};
 const valid=sources.length>0&&(mode==='batch'?sources.every(source=>draftValid(batchDrafts[source.path]??emptyDraft(fileName(source.path).replace(/\.zip$/i,'')))):draftValid(draft))&&!batchInvalid;
 const dirty=!!(sources.length||name.trim()||tags.length||pendingTag.trim()||sourceUrl.trim()||thumbnail||category!=='NonVRChat'||mode!=='single');
 useEffect(()=>{c.onDirtyChange?.(dirty);},[dirty,c.onDirtyChange]);
 useEffect(()=>()=>c.onDirtyChange?.(false),[c.onDirtyChange]);
 async function native(service:'storage'|'dialog'|'booth',action:string,args:Record<string,unknown>={}){
  if(!c.onNative)throw new Error('nativeUnavailable');
  return c.onNative(service,action,args);
 }
 async function run(action:()=>Promise<void>){
  if(lock.current)return;
  lock.current=true;setBusy(true);setError('');
  try{await action();}catch(e){setError(errorCode(e));}finally{lock.current=false;setBusy(false);}
 }
 function addSources(paths:string[],kind:Source['kind']){
  if(paths.some(path=>!absolutePath(path)))throw new Error('root_not_absolute');
  setSources(previous=>[...previous,...paths.filter(path=>!previous.some(source=>source.path===path)).map(path=>({path,kind}))]);
  setBatchDrafts(current=>({...Object.fromEntries(paths.map(path=>[path,emptyDraft(fileName(path).replace(/\.zip$/i,''))])),...current}));
  if(mode==='single'&&paths[0])setSingleDraft(current=>current.name.trim()?current:{...current,name:kind==='folder'?fileName(paths[0]):fileName(paths[0]).replace(/\.[^.]+$/,'')});
 }
 useEffect(()=>{if(!isTauri())return;let stop:(()=>void)|undefined;let alive=true;void getCurrentWebview().onDragDropEvent(event=>{if(!alive||lock.current)return;const payload=event.payload;setDropping(payload.type==='enter'||payload.type==='over');if(payload.type==='drop'){setDropping(false);try{addSources(payload.paths,'unknown');}catch(error){setError(errorCode(error));}}}).then(unlisten=>{if(alive)stop=unlisten;else unlisten();}).catch(()=>{});return()=>{alive=false;stop?.();};},[mode]);
 const fetchMetadata=()=>run(async()=>{if(!sourceUrl.trim())throw new Error('invalidURL');const data=await native('booth','fetch_product_metadata',{url:sourceUrl.trim()});if(!data||typeof data.title!=='string'||!categoryKeys.includes(data.category)||!Array.isArray(data.tags))throw new Error('invalidMetadata');updateDraft({name:data.title,category:data.category,categoryManual:true,tags:data.tags,sourceUrl:data.sourceUrl,thumbnail:null,thumbnailUrl:data.thumbnailUrl??''});});
 useEffect(()=>{if(seed?.sources?.length)addSources(seed.sources,'unknown');},[seed?.sources]);
 const chooseSources=(folder:boolean)=>run(async()=>{
  const result=await native('dialog',folder?'folder':'open_file',folder?{multiple:true}:{multiple:true,...(mode==='batch'?{filters:[{name:'ZIP',extensions:['zip']}]}:{})});
  addSources(selectedPaths(result),folder?'folder':'file');
 });
 const chooseThumbnail=()=>run(async()=>{
  const [path]=selectedPaths(await native('dialog','open_file',{filters:[{name:L('thumbnail'),extensions:['png','jpg','jpeg','gif','webp']}]}));
  if(!path)return;
  const copied=await nativeFileAction<{relativePath:string;path:string}>('copy_image',{path});
  setThumbnail({...copied,name:fileName(path)});
 });
 const chooseLibrary=()=>run(async()=>{
  const [path]=selectedPaths(await native('dialog','folder'));
  if(path)setLibraryPath(path);
 });
 const submit=()=>run(async()=>{
  if(!valid||accepted.current)return;
  const destinationRoot=destinationRootId;
  if(!ready){
   if(!libraryGate){setLibraryGate(true);return;}
   if(!absolutePath(libraryPath))throw new Error('root_not_absolute');
   if(c.onLibraryReady)await c.onLibraryReady(libraryPath,destinationRoot,true);
   else await native('storage','configure_root',{path:libraryPath});
  }
  if(!mounted.current)return;
  const allTags=[...tags,...(pendingTag.trim()?[resolveTag(pendingTag,suggestions,aliases)]:[])];
  const seen=new Set<string>();
  const uniqueTags=allTags.filter(tag=>{const key=normalizeSearch(tag);if(!key||seen.has(key))return false;seen.add(key);return true;});
  const products=mode==='batch'?sources.map(source=>{const item=batchDrafts[source.path]??emptyDraft(fileName(source.path).replace(/\.zip$/i,''));return {source:source.path,name:item.name.trim(),category:item.category,categoryManual:item.categoryManual,tags:[...new Set([...item.tags,...(item.pendingTag.trim()?[resolveTag(item.pendingTag,suggestions,aliases)]:[])])],sourceUrl:item.sourceUrl.trim(),...(item.thumbnail?{thumbnailPath:item.thumbnail.relativePath}:{}),thumbnailUrl:item.thumbnailUrl};}):undefined;
  const result=await native('storage','import_local',{draftId:draftId.current,mode,sources:sources.map(source=>source.path),...(destinationRoot?{rootId:destinationRoot}:{}),name:name.trim(),category,categoryManual,tags:uniqueTags,...(sourceUrl.trim()?{sourceUrl:sourceUrl.trim()}:{}),...(thumbnail?{thumbnailPath:thumbnail.relativePath}:{}),thumbnailUrl,...(products?{products}:{})});
  if(typeof result?.jobId!=='string')throw new Error('operationFailed');
  accepted.current=true;
  c.onDirtyChange?.(false);
  const productIds=Array.isArray(result.productIds)?result.productIds.filter((id:unknown):id is string=>typeof id==='string'):[];
  c.onNewAssets?.(productIds,mode==='single'?category:undefined,result.jobId);
  c.onNotice(L('importQueued'));
  c.onClose();
 });
 const errorView=error&&<div className="wf-error" role="alert">{L(error)}</div>;
 if(libraryGate)return <><div className="wf-scroll"><Field label={L('libraryLocation')}><div className="wf-location-choice"><input autoFocus readOnly value={libraryPath}/><button disabled={busy} onClick={chooseLibrary}>{L('chooseLocation')}</button></div></Field>{errorView}</div><footer className="wf-actions"><button disabled={busy} onClick={c.onClose}>{L('cancel')}</button><button disabled={busy} onClick={()=>setLibraryGate(false)}>{L('back')}</button><button className="primary" disabled={busy||!absolutePath(libraryPath)} onClick={submit}>{L(busy?'running':'importNow')}</button></footer></>;
 return <><div className="wf-scroll">
  <div className="wf-source-mode">{(['single','batch'] as const).map(value=><label key={value}><input type="radio" name="local-import-mode" checked={mode===value} disabled={busy} onChange={()=>setMode(value)}/>{L(value==='single'?'singleProduct':'batchProducts')}</label>)}</div>
  <div className={`local-import-columns ${mode==='batch'?'is-batch':''}`}><div className="local-import-sources">
  <span className="wf-field-label">{L('filesOrFolder')}</span>
  <div className={`data-list local-import-drop ${dropping?'is-dropping':''}`} tabIndex={0} role="group" aria-label={L('filesOrFolder')} onDragOver={event=>{event.preventDefault();setDropping(true);}} onDragLeave={()=>setDropping(false)} onDrop={event=>{event.preventDefault();setDropping(false);}}><div className="local-import-drop-label">{L('dropFiles')}</div>{sources.map(source=><div className="data-row" key={source.path}>{mode==='batch'?<button className="local-import-product" aria-pressed={editorPath===source.path} disabled={busy} title={source.path} onClick={()=>setSelectedPath(source.path)}>{(batchDrafts[source.path]?.name||fileName(source.path))}</button>:<span title={source.path}>{fileName(source.path)}</span>}<button disabled={busy} aria-label={`${L('remove')} ${fileName(source.path)}`} onClick={()=>setSources(previous=>previous.filter(item=>item.path!==source.path))}>×</button></div>)}</div>
  <div className="form-row"><button disabled={busy} onClick={()=>chooseSources(false)}>{L('addFiles')}</button><button disabled={busy} onClick={()=>chooseSources(true)}>{L('addFolder')}</button></div>
  </div><div className="local-import-editor" inert={mode==='batch'&&!sources.length}>
  {batchInvalid&&<div className="wf-error" role="alert">{L('local_batch_requires_folder_or_zip')}</div>}
  {roots.length>1&&<Field label={L('currentLocation')}><select value={rootId||defaultRootId} disabled={busy} onChange={e=>setRootId(e.target.value)}>{roots.map(root=><option key={root.id} value={root.id}>{root.label??root.path}</option>)}</select></Field>}
  <Field label={L('productURL')}><div className="local-import-url"><input value={sourceUrl} disabled={busy} placeholder="https://" onChange={e=>setSourceUrl(e.target.value)}/><button disabled={busy||!sourceUrl.trim()||!validUrl} aria-busy={busy} onClick={fetchMetadata}>{L(busy?'running':'fetch')}</button></div></Field>
  <Field label={L('name')}><input value={name} disabled={busy} onChange={e=>setName(e.target.value)}/></Field>
  {!validUrl&&<div className="wf-error" role="alert">{L('invalidURL')}</div>}
  <Field label={L('category')}><select value={category} disabled={busy} onChange={e=>setCategory(e.target.value)}>{categoryKeys.map(key=><option key={key} value={key}>{t(c.locale,key)}</option>)}</select></Field>
  <div className="wf-import-extras"><div className="wf-thumbnail"><span>{L('thumbnail')}</span><button className="wf-thumbnail-pick" disabled={busy} aria-label={L('thumbnail')} onClick={chooseThumbnail}><Preview src={thumbnail?previewSource(thumbnail.path):previewSource(thumbnailUrl)} title={L('thumbnail')} placeholderTitle="＋"/></button><button disabled={busy} onClick={chooseThumbnail}>{L('chooseFile')}</button>{thumbnail&&<button disabled={busy} title={thumbnail.name} aria-label={L('remove')} onClick={()=>setThumbnail(null)}>×</button>}</div><TagPicker key={mode==='batch'?editorPath:'single'} initialQuery={pendingTag} locale={c.locale} label={L('tags')} value={tags} suggestions={suggestions} aliases={aliases} onChange={value=>{if(!lock.current)setTags(value);}} onDraftChange={value=>{if(!lock.current)setPendingTag(value);}}/></div>
  </div></div>{errorView}
 </div><footer className="wf-actions"><button disabled={busy} onClick={c.onClose}>{L('cancel')}</button><button className="primary" disabled={busy||!valid} onClick={submit}>{L(busy?'running':'importNow')}</button></footer></>;
}
