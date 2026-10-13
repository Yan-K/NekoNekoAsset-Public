import {useEffect,useRef,useState} from 'react';
import {isTauri} from '@tauri-apps/api/core';
import {Download,RefreshCw,X} from 'lucide-react';
import type {Locale} from './model';
import {checkUpdates,errorCode,installUpdate,openExternal,subscribeUpdateProgress,type UpdateCheck,type UpdateProgress} from './backend';
import {updateText} from './update-i18n';
import './update.css';

export default function UpdatePrompt({locale,currentVersion,ready,canPrompt,installBlocked,open,request,onOpenChange,onAvailableChange,onBeforeInstall}:{
 locale:Locale;currentVersion:string;ready:boolean;canPrompt:boolean;installBlocked:boolean;open:boolean;request:number;
 onOpenChange:(open:boolean)=>void;onAvailableChange:(available:boolean)=>void;onBeforeInstall:()=>Promise<void>;
}){
 const L=(key:string)=>updateText(locale,key);
 const [result,setResult]=useState<UpdateCheck|null>(null),[checking,setChecking]=useState(false),[installing,setInstalling]=useState(false),[error,setError]=useState(''),[progress,setProgress]=useState<UpdateProgress|null>(null),[pendingPrompt,setPendingPrompt]=useState(false);
 const started=useRef(false),mounted=useRef(false),operation=useRef(false),lastRequest=useRef(0),sequence=useRef(0),box=useRef<HTMLDivElement>(null),openRef=useRef(open);openRef.current=open;
 useEffect(()=>{mounted.current=true;return()=>{mounted.current=false;};},[]);
 async function check(automatic=false){
  if(operation.current)return;operation.current=true;const generation=++sequence.current;setChecking(true);setError('');
  try{const value=await checkUpdates();if(!mounted.current||generation!==sequence.current)return;setResult(value);onAvailableChange(!!value.app.available);if(value.app.errorCode)setError(value.app.errorCode);if(automatic&&value.app.available&&!openRef.current)setPendingPrompt(true);}
  catch(error){if(mounted.current)setError(errorCode(error));}
  finally{operation.current=false;if(mounted.current)setChecking(false);}
 }
 useEffect(()=>{if(ready&&isTauri()&&!started.current){started.current=true;void check(true);}},[ready]);
 useEffect(()=>{if(request!==lastRequest.current){lastRequest.current=request;setPendingPrompt(false);void check();}},[request]);
 useEffect(()=>{if(pendingPrompt&&canPrompt&&!open){setPendingPrompt(false);onOpenChange(true);}},[pendingPrompt,canPrompt,open]);
 useEffect(()=>{
  if(!open)return;const previous=document.activeElement as HTMLElement|null;
  box.current?.querySelector<HTMLElement>('button:not(:disabled)')?.focus();
  return()=>{previous?.focus();};
 },[open]);
 useEffect(()=>{if(installing)box.current?.focus();},[installing]);
 async function install(){
  const available=result?.app.available;if(!available||installBlocked||operation.current)return;
  operation.current=true;setInstalling(true);setError('');setProgress(null);let stop:(()=>void)|undefined;
  try{await onBeforeInstall();stop=await subscribeUpdateProgress(value=>{if(mounted.current)setProgress(value);});await installUpdate(available.version);}
  catch(error){if(mounted.current)setError(errorCode(error));}
  finally{stop?.();operation.current=false;if(mounted.current){setInstalling(false);setProgress(null);}}
 }
 if(!open)return null;
 const available=result?.app.available,stage=progress?.stage??'downloading',percent=progress?.totalBytes&&progress.totalBytes>0?Math.min(100,100*progress.downloadedBytes/progress.totalBytes):undefined;
 const close=()=>{if(!installing)onOpenChange(false);};
 return <div className="modal-backdrop" onMouseDown={event=>{if(event.target===event.currentTarget)close();}}><div className="modal update-modal" role="dialog" aria-modal="true" aria-busy={installing||checking} aria-labelledby="app-update-title" tabIndex={-1} ref={box} onKeyDown={event=>{
  if(event.key==='Escape'&&!event.nativeEvent.isComposing){event.stopPropagation();event.preventDefault();close();}
  if(event.key==='Tab'){const elements=Array.from(box.current?.querySelectorAll<HTMLElement>('button:not(:disabled),a[href],[tabindex="0"]')??[]).filter(item=>item.getClientRects().length);const first=elements[0],last=elements.at(-1);if(!elements.length){event.preventDefault();}else if(event.shiftKey&&(document.activeElement===first||document.activeElement===box.current)){event.preventDefault();last?.focus();}else if(!event.shiftKey&&(document.activeElement===last||document.activeElement===box.current)){event.preventDefault();first?.focus();}}
 }}><header className="modal-header"><h2 id="app-update-title">{L('title')}</h2><button type="button" disabled={installing} aria-label={L('close')} onClick={close}><X size={15}/></button></header>
 <div className="modal-body update-body"><div className="update-current"><span>{L('current')}</span><strong>v{result?.app.currentVersion??currentVersion}</strong></div>
 {installing?<div className="update-progress" role="status"><span>{L(stage)}</span><progress max={100} value={stage==='downloading'?percent:undefined} aria-label={L(stage)}/>{stage==='downloading'&&percent!==undefined&&<small>{Math.round(percent)}%</small>}</div>:checking?<div className="update-status" role="status"><RefreshCw size={16} className="sync-spinning"/>{L('checking')}</div>:available?<><strong className="update-status">{L('available').replace('{version}',available.version)}</strong>{available.notes&&<div className="update-notes">{available.notes}</div>}<button type="button" className="update-release-link" onClick={()=>void openExternal(available.releaseUrl).catch(error=>setError(errorCode(error)))}>{L('release')}</button>{!result.app.installSupported?<p className="update-blocked">{L('updates_portable_unsupported')}</p>:installBlocked&&<p className="update-blocked">{L('busy')}</p>}</>:result&&!error?<div className="update-status" role="status">{L('latest')}</div>:null}
 {error&&<div className="wf-error" role="alert">{L(error)}</div>}</div>
 <footer className="modal-footer"><button type="button" disabled={installing||checking} onClick={()=>void check()}><RefreshCw size={14}/>{L('check')}</button><span className="spacer"/><button type="button" disabled={installing} onClick={close}>{L(available?'later':'close')}</button>{available&&result?.app.installSupported&&<button type="button" className="primary" disabled={installing||checking||installBlocked} onClick={()=>void install()}><Download size={14}/>{L('install')}</button>}</footer></div></div>;
}
