import {useEffect,useMemo,useRef,useState} from 'react';
import {RefreshCw,Download} from 'lucide-react';
import type {Locale,Product} from './model';
import {checkUpdates,openExternal,type BridgeRelease,errorCode,progressPercent,listUnityEditors,unityInventory,submitUnity,unityDetails,queryUnity,cancelUnity,type UnityEditor,type UnityLeaf,type UnityDetails} from './backend';
import {unityText,unityError,unityProgressLabel} from './unity-i18n';
import './UnityWorkflow.css';
import './update.css';
import {isNewerBridge,updateText} from './update-i18n';

export interface UnitySeed {productIds:string[];leafRefs?:string[]}
export default function UnityWorkflow({locale,products,seed,onClose,onSubmitted,onBridgeVersionChange}:{locale:Locale;products:Product[];seed:UnitySeed;onClose:()=>void;onSubmitted:(jobId:string)=>void;onBridgeVersionChange?:(version:string|null)=>void}){
 const L=(key:string)=>unityText(locale,key);
 const [editors,setEditors]=useState<UnityEditor[]>([]),[files,setFiles]=useState<UnityLeaf[]>([]),[selected,setSelected]=useState<Set<string>>(new Set());
 const [target,setTarget]=useState(''),[loading,setLoading]=useState(true),[sending,setSending]=useState(false),[error,setError]=useState('');
 const [revision,setRevision]=useState(0),[bridgeRelease,setBridgeRelease]=useState<BridgeRelease|null>(null),[bridgeChecking,setBridgeChecking]=useState(false),[bridgeError,setBridgeError]=useState('');
 const bridgeCheckLock=useRef(false);
 async function checkBridge(){if(bridgeCheckLock.current)return;bridgeCheckLock.current=true;setBridgeChecking(true);setBridgeError('');try{const value=await checkUpdates();setBridgeRelease(value.bridge??null);setBridgeError(value.bridgeError??'');}catch(error){setBridgeError(errorCode(error));}finally{bridgeCheckLock.current=false;setBridgeChecking(false);}}
 useEffect(()=>{void checkBridge();},[]);
 const key=JSON.stringify(seed);
 const initialized=useRef('');
 useEffect(()=>{
  let stopped=false;setLoading(true);setError('');
  void Promise.all([listUnityEditors(),unityInventory(seed.productIds)]).then(([live,inventory])=>{
   if(stopped)return;setEditors(live.editors);setFiles(inventory.files);
   setTarget(previous=>live.editors.some(editor=>editor.sessionId===previous)?previous:live.editors.find(editor=>!editor.busy)?.sessionId??'');
   const first=initialized.current!==key;initialized.current=key;
   setSelected(previous=>new Set(inventory.files.filter(file=>first?(seed.leafRefs?seed.leafRefs.includes(file.leafRef):file.kind==='unitypackage'):previous.has(file.leafRef)).map(file=>file.leafRef)));
  }).catch(error=>{if(!stopped)setError(errorCode(error));}).finally(()=>{if(!stopped)setLoading(false);});
  return()=>{stopped=true;};
 },[key,revision]);
 const editor=editors.find(editor=>editor.sessionId===target);
 useEffect(()=>{onBridgeVersionChange?.(editor?.bridgeVersion??null);},[editor?.bridgeVersion,onBridgeVersionChange]);
 const orderedFiles=useMemo(()=>{const order=new Map(seed.productIds.map((id,index)=>[id,index])),names=new Intl.Collator(locale,{numeric:true,sensitivity:'base'});return [...files].sort((a,b)=>(order.get(a.productId)??0)-(order.get(b.productId)??0)||Number(b.kind==='unitypackage')-Number(a.kind==='unitypackage')||names.compare(a.relativePath,b.relativePath)||names.compare(a.variantName,b.variantName)||names.compare(a.sourceName,b.sourceName));},[files,key,locale]);
 const groups=useMemo(()=>{const result=new Map<string,{id:string;title:string;files:UnityLeaf[]}>();for(const file of orderedFiles){let group=result.get(file.productId);if(!group){group={id:file.productId,title:products.find(product=>product.id===file.productId)?.title??file.productTitle,files:[]};result.set(file.productId,group);}group.files.push(file);}return [...result.values()];},[orderedFiles,products]);
 const duplicate=editor&&editors.filter(item=>item.projectName===editor.projectName).length>1;
 async function send(){
  if(!editor)return;setSending(true);setError('');
  try{const result=await submitUnity({locale,productIds:seed.productIds,leafRefs:orderedFiles.filter(file=>selected.has(file.leafRef)).map(file=>file.leafRef),instanceId:editor.instanceId,sessionId:editor.sessionId});onSubmitted(result.jobId);}catch(error){setError(errorCode(error));setSending(false);}
 }
 const bytes=(size:number)=>{const unit=size>=1024**3?3:size>=1024**2?2:size>=1024?1:0;return new Intl.NumberFormat(locale,{maximumFractionDigits:1}).format(size/1024**unit)+' '+['B','KB','MB','GB'][unit];};
 const fileList=(kind:UnityLeaf['kind'])=>{
  const columns=groups.map(group=>({...group,files:group.files.filter(file=>file.kind===kind)})).filter(group=>group.files.length),available=columns.flatMap(group=>group.files);
  return <><div className="unity-file-heading"><strong>{L(kind==='unitypackage'?'packages':'singleFiles')}</strong><span>{L('selected')} · {available.filter(file=>selected.has(file.leafRef)).length.toLocaleString(locale)} / {available.length.toLocaleString(locale)}</span><label className="unity-select-all"><input type="checkbox" disabled={loading||sending||!available.length} checked={!!available.length&&available.every(file=>selected.has(file.leafRef))} ref={input=>{if(input)input.indeterminate=available.some(file=>selected.has(file.leafRef))&&!available.every(file=>selected.has(file.leafRef));}} onChange={event=>{const checked=event.target.checked;setSelected(previous=>{const next=new Set(previous);for(const file of available){if(checked)next.add(file.leafRef);else next.delete(file.leafRef);}return next;});}}/>{L('selectAll')}</label></div>
   {loading?<progress aria-label={L('preparing')}/>:!available.length?<p className="status-line">{L('noFiles')}</p>:<div className="unity-file-list">{columns.map(group=><section key={group.id}><h3 title={group.title}>{group.title}</h3>{group.files.map(file=><label className="unity-file-row" key={file.leafRef}><input type="checkbox" disabled={sending} checked={selected.has(file.leafRef)} onChange={event=>setSelected(previous=>{const next=new Set(previous);if(event.target.checked)next.add(file.leafRef);else next.delete(file.leafRef);return next;})}/><span className="unity-file-name"><span title={file.relativePath}>{file.relativePath}</span>{(file.variantName||file.memberPath&&file.sourceName)&&<small>{[file.variantName,file.memberPath?file.sourceName:''].filter(Boolean).join(' · ')}</small>}</span><small className="unity-file-size">{bytes(file.bytes)}</small></label>)}</section>)}</div>}
  </>;
 };
 return <><div className="wf-scroll unity-picker"><div className="unity-columns">
  <div className="unity-column unity-packages-column"><div className="unity-target-row"><label className="field"><span>{L('project')}</span><select aria-label={L('project')} disabled={loading||sending||!editors.length} value={target} onChange={event=>setTarget(event.target.value)}><option value="">—</option>{editors.map(editor=><option key={editor.sessionId} value={editor.sessionId} disabled={editor.busy}>{editor.projectName} · {editor.unityVersion}{editors.filter(item=>item.projectName===editor.projectName).length>1?' · '+editor.projectRoot:''}{editor.busy?' · '+L('busy'):''}</option>)}</select></label><button type="button" aria-label={L('refresh')} disabled={loading||sending} onClick={()=>setRevision(value=>value+1)}><RefreshCw size={15} className={loading?'sync-spinning':undefined}/></button></div>
   {duplicate&&<small className="unity-target-path" title={editor.projectRoot}>{editor.projectRoot}</small>}
   {!loading&&!editors.length&&<p className="status-line">{L('noEditors')}</p>}
   <div className="unity-bridge-download"><button type="button" disabled={sending} onClick={()=>void openExternal(bridgeRelease?.unitypackageUrl??'https://github.com/Yan-K/NekoNekoAsset-Public/releases').catch(error=>setError(errorCode(error)))}><Download size={13}/>{updateText(locale,'bridgeDownload')}</button>{bridgeRelease&&isNewerBridge(bridgeRelease.version,editor?.bridgeVersion)&&<small className="bridge-update">{updateText(locale,'bridgeUpdate').replace('{version}',bridgeRelease.version)}</small>}{editor&&!editor.bridgeVersion&&<small>{updateText(locale,'bridgeUnknown')}</small>}{bridgeChecking&&<RefreshCw size={13} className="sync-spinning"/>}{bridgeError&&<><small role="status">{updateText(locale,'bridgeFailed')}</small><button type="button" disabled={bridgeChecking||sending} onClick={()=>void checkBridge()}>{updateText(locale,'retry')}</button></>}</div>
   {fileList('unitypackage')}
  </div><div className="unity-column unity-files-column">{fileList('file')}</div>
 </div>
  {error&&<div className="wf-error" role="alert">{unityError(locale,error)}</div>}
 </div><footer className="wf-actions"><button type="button" onClick={onClose}>{L('close')}</button><button type="button" className="unity-action" disabled={loading||sending||!editor||editor.busy||!selected.size} onClick={()=>void send()}>{L('send')}</button></footer></>;
}

export function UnityResults({locale,jobId,onClose}:{locale:Locale;jobId:string;onClose:()=>void}){
 const L=(key:string)=>unityText(locale,key);const [detail,setDetail]=useState<UnityDetails|null>(null),[error,setError]=useState(''),[busy,setBusy]=useState(false),[limit,setLimit]=useState(100);
 useEffect(()=>{let current=true,timer:ReturnType<typeof setTimeout>;const load=async()=>{let finished=false;try{const value=await unityDetails(jobId);if(current){setDetail(value);setError('');}finished=['completed','failed','cancelled','interrupted'].includes(value.stage);}catch(error){if(current)setError(errorCode(error));}if(current&&!finished)timer=setTimeout(()=>void load(),2000);};void load();return()=>{current=false;clearTimeout(timer);};},[jobId]);
 async function update(cancel=false){setBusy(true);setError('');try{setDetail(await (cancel?cancelUnity(jobId):queryUnity(jobId)));}catch(error){setError(errorCode(error));}finally{setBusy(false);}}
 const receipt=detail?.receipt,active=detail&&!['completed','failed','cancelled','interrupted'].includes(detail.stage);
 return <><div className="wf-scroll unity-results"><div className="unity-result-heading"><strong>{detail?.projectName}</strong><span>{detail?.unityVersion} · {unityProgressLabel(locale,{...receipt,stage:detail?.stage})}</span></div>{(error||detail?.errorCode)&&<div className="wf-error" role="alert">{unityError(locale,error||detail?.errorCode||'')}</div>}{active&&<progress aria-label={L('preparing')} value={progressPercent({id:jobId,type:'unity:import',state:'running',stage:detail.stage,...receipt})} max={100}/>} {receipt?.error&&<div className="wf-error">{receipt.error}</div>}{!!receipt?.compileErrors.length&&<details open><summary>{L('compileErrors')}</summary><ul className="unity-compile-errors">{receipt.compileErrors.map((message,index)=><li key={index}>{message}</li>)}</ul></details>}{receipt?.results.length?<><div className="unity-result-list">{receipt.results.slice(0,limit).map((result,index)=><div className={'unity-result-row '+result.state} key={result.entryId+':'+index}><span title={result.path}>{result.path}</span><small>{L(result.state)}</small>{result.message&&<small>{result.message}</small>}</div>)}</div>{receipt.results.length>limit&&<button type="button" onClick={()=>setLimit(value=>value+100)}>+ {(receipt.results.length-limit).toLocaleString(locale)}</button>}</>:<p className="status-line">{L('noResults')}</p>}</div><footer className="wf-actions">{active&&!receipt?.writesStarted&&<button type="button" disabled={busy} onClick={()=>void update(true)}>{L('cancel')}</button>}{active&&<button type="button" disabled={busy} onClick={()=>void update()}>{L('query')}</button>}<button type="button" onClick={onClose}>{L('close')}</button></footer></>;
}
