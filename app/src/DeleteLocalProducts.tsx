import {useEffect,useLayoutEffect,useRef,useState} from 'react';
import type {Locale} from './model';
import {shellText} from './shell-i18n';
import {t} from './i18n';
import {nativeErrorText} from './error-i18n';
import {errorCode} from './backend';

export default function DeleteLocalProducts({locale,ids,names,onDelete,onClose}:{locale:Locale;ids:string[];names:string[];onDelete:(ids:string[])=>Promise<void>;onClose:()=>void}){
 const L=(key:string)=>shellText(locale,key)??t(locale,key);
 const [busy,setBusy]=useState(false),[error,setError]=useState(''),[recoveryRequired,setRecoveryRequired]=useState(false);
 const box=useRef<HTMLDivElement>(null),cancel=useRef<HTMLButtonElement>(null),inFlight=useRef(false),close=useRef(onClose),previousFocus=useRef(document.activeElement);close.current=onClose;
 useEffect(()=>{
  const previous=previousFocus.current;cancel.current?.focus();
  const key=(event:KeyboardEvent)=>{
   if(event.isComposing)return;
   if(event.key==='Escape'){event.preventDefault();event.stopPropagation();if(!inFlight.current)close.current();}
   if(event.key==='Tab'){
    const nodes=[...box.current?.querySelectorAll<HTMLButtonElement>('button:not(:disabled)')??[]];
    if(!nodes.length){event.preventDefault();box.current?.focus();return;}
    if(event.shiftKey&&(document.activeElement===nodes[0]||!nodes.includes(document.activeElement as HTMLButtonElement))){event.preventDefault();nodes.at(-1)?.focus();}
    else if(!event.shiftKey&&(document.activeElement===nodes.at(-1)||!nodes.includes(document.activeElement as HTMLButtonElement))){event.preventDefault();nodes[0]?.focus();}
   }
  };
  document.addEventListener('keydown',key,true);
  return()=>{document.removeEventListener('keydown',key,true);if(previous instanceof HTMLElement&&previous.isConnected)previous.focus({preventScroll:true});};
 },[]);
 useLayoutEffect(()=>{if(busy)box.current?.focus();else cancel.current?.focus();},[busy]);
 async function remove(){
  if(inFlight.current)return;inFlight.current=true;setBusy(true);setError('');box.current?.focus();
  try{await onDelete(ids);onClose();}
  catch(error){const code=errorCode(error);setRecoveryRequired(['local_delete_cleanup_failed','local_delete_rollback_failed','local_delete_retry_required'].includes(code.split('.').at(-1)??code));setError(nativeErrorText(locale,code)??L('operationFailed'));}
  finally{inFlight.current=false;setBusy(false);}
 }
 return <div className="modal-backdrop" onMouseDown={event=>{if(event.target===event.currentTarget&&!inFlight.current)onClose();}}><div className="modal" role="alertdialog" aria-modal="true" aria-labelledby="local-delete-title" aria-describedby="local-delete-message" aria-busy={busy} tabIndex={-1} ref={box}>
  <header className="modal-header"><h2 id="local-delete-title">{L('deleteLocalProducts')}</h2></header>
  <div className="modal-body"><p id="local-delete-message">{L('confirmDeleteLocalProducts').replace('{count}',ids.length.toLocaleString(locale))}</p>{names.length>0&&<ul style={{margin:0,paddingLeft:20,overflowWrap:'anywhere'}}>{names.slice(0,8).map((name,index)=><li key={index}>{name}</li>)}</ul>}{ids.length>8&&<p>+ {(ids.length-8).toLocaleString(locale)}</p>}{error&&<div className="callout error" role="alert">{error}</div>}</div>
  <footer className="modal-footer"><button type="button" ref={cancel} disabled={busy} onClick={onClose}>{L(recoveryRequired?'close':'cancel')}</button>{!recoveryRequired&&<button type="button" className="danger" disabled={busy} onClick={()=>void remove()}>{L(busy?'processing':'delete')}</button>}</footer>
 </div></div>;
}
