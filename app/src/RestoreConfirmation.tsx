import {useEffect,useLayoutEffect,useRef,useState} from 'react';
import {createPortal} from 'react-dom';
import type {Locale} from './model';
import {errorCode} from './backend';
import {nativeWorkflowText} from './native-workflow-i18n';
import './settings-reset.css';

export default function RestoreConfirmation({locale,path,onRestore,onClose}:{locale:Locale;path:string;onRestore:()=>Promise<unknown>;onClose:()=>void}) {
 const [busy,setBusy]=useState(false),[error,setError]=useState('');
 const box=useRef<HTMLDivElement>(null),cancel=useRef<HTMLButtonElement>(null),inFlight=useRef(false),close=useRef(onClose);close.current=onClose;
 const L=(key:string)=>nativeWorkflowText(locale,key);
 useLayoutEffect(()=>{const previous=document.activeElement as HTMLElement;return()=>{if(previous?.isConnected)previous.focus({preventScroll:true});};},[]);
 useEffect(()=>{
  const key=(event:KeyboardEvent)=>{
   if(event.isComposing||event.keyCode===229)return;
   if(event.ctrlKey||event.metaKey){event.stopPropagation();if(['f','z','a','+','=','-','add','subtract'].includes(event.key.toLowerCase()))event.preventDefault();}
   if(event.key==='Escape'){event.preventDefault();event.stopPropagation();if(!inFlight.current)close.current();}
   if(event.key==='Tab'){
    event.stopPropagation();const nodes=[...box.current?.querySelectorAll<HTMLButtonElement>('button:not(:disabled)')??[]];
    const index=nodes.indexOf(document.activeElement as HTMLButtonElement);
    if(!nodes.length){event.preventDefault();box.current?.focus();}
    else if(event.shiftKey&&index<=0){event.preventDefault();nodes.at(-1)?.focus();}
    else if(!event.shiftKey&&(index<0||index===nodes.length-1)){event.preventDefault();nodes[0]?.focus();}
   }
  };
  document.addEventListener('keydown',key,true);return()=>document.removeEventListener('keydown',key,true);
 },[]);
 useLayoutEffect(()=>{if(busy)box.current?.focus();else cancel.current?.focus();},[busy]);
 async function restore(){if(inFlight.current)return;inFlight.current=true;setBusy(true);setError('');try{await onRestore();onClose();}catch(error){setError(errorCode(error));inFlight.current=false;setBusy(false);}}
 return createPortal(<div className="modal-backdrop reset-app-backdrop" onMouseDown={event=>{event.stopPropagation();if(event.target===event.currentTarget&&!inFlight.current)onClose();}} onKeyDown={event=>event.stopPropagation()}><div className="modal reset-app-modal" role="alertdialog" aria-modal="true" aria-labelledby="restore-backup-title" aria-describedby="restore-backup-message restore-backup-path" aria-busy={busy} tabIndex={-1} ref={box}><header className="modal-header"><h2 id="restore-backup-title">{L('restore')}</h2></header><div className="modal-body"><p id="restore-backup-message">{L('restoreBackupMessage')}</p><p id="restore-backup-path">{path}</p>{error&&<div className="callout error" role="alert">{L(error)}</div>}</div><footer className="modal-footer"><button ref={cancel} disabled={busy} onClick={onClose}>{L('cancel')}</button><button className="danger" disabled={busy} onClick={()=>void restore()}>{L('restore')}</button></footer></div></div>,document.querySelector('.app')??document.body);
}
