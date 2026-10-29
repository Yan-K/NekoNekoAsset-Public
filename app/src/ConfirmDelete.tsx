import {useEffect,useLayoutEffect,useRef,useState} from 'react';
import {createPortal} from 'react-dom';
import type {Locale} from './model';
import {errorCode} from './backend';
import {nativeErrorText} from './error-i18n';
import {editorText} from './editor-i18n';
import './editor-styles.css';

export default function ConfirmDelete({locale,kind,name,onDelete,onClose}:{locale:Locale;kind:'collection'|'search'|'theme';name:string;onDelete:()=>Promise<void>;onClose:()=>void}) {
 const [busy,setBusy]=useState(false),[error,setError]=useState('');
 const box=useRef<HTMLDivElement>(null),cancel=useRef<HTMLButtonElement>(null),inFlight=useRef(false),close=useRef(onClose);close.current=onClose;
 const [previous]=useState(()=>document.activeElement instanceof HTMLElement?document.activeElement:null);
 const [fallback]=useState(()=>previous?.closest('.modal,.appearance-content')??null);
 const L=(key:string)=>editorText(locale,key);
 useLayoutEffect(()=>()=>{const target=previous?.isConnected&&!previous.matches(':disabled')?previous:fallback?.querySelector<HTMLElement>('input:not(:disabled),button:not(:disabled),select:not(:disabled)');if(target?.isConnected)requestAnimationFrame(()=>target.focus({preventScroll:true}));},[previous,fallback]);
 useEffect(()=>{
  const key=(event:KeyboardEvent)=>{
   if(event.isComposing||event.keyCode===229)return;
   if(event.ctrlKey||event.metaKey){event.stopPropagation();if(['f','z','a','+','=','-','add','subtract'].includes(event.key.toLowerCase()))event.preventDefault();}
   if(event.key==='Escape'){event.preventDefault();event.stopPropagation();if(!inFlight.current)close.current();}
   if(event.key==='Tab'){
    event.stopPropagation();const nodes=[...box.current?.querySelectorAll<HTMLButtonElement>('button:not(:disabled)')??[]],index=nodes.indexOf(document.activeElement as HTMLButtonElement);
    if(!nodes.length){event.preventDefault();box.current?.focus();}
    else if(event.shiftKey&&index<=0){event.preventDefault();nodes.at(-1)?.focus();}
    else if(!event.shiftKey&&(index<0||index===nodes.length-1)){event.preventDefault();nodes[0]?.focus();}
   }
  };
  document.addEventListener('keydown',key,true);return()=>document.removeEventListener('keydown',key,true);
 },[]);
 useLayoutEffect(()=>{if(busy)box.current?.focus();else cancel.current?.focus();},[busy]);
 async function remove(){if(inFlight.current)return;inFlight.current=true;setBusy(true);setError('');try{await onDelete();onClose();}catch(error){const code=errorCode(error);setError(code==='entryUnavailable'?L(code):nativeErrorText(locale,code)??L('operationFailed'));inFlight.current=false;setBusy(false);}}
 return createPortal(<div className="modal-backdrop editor-confirm-backdrop" onMouseDown={event=>{event.stopPropagation();if(event.target===event.currentTarget&&!inFlight.current)onClose();}} onKeyDown={event=>event.stopPropagation()}><div className="modal editor-confirm" role="alertdialog" aria-modal="true" aria-labelledby="confirm-delete-title" aria-describedby="confirm-delete-message" aria-busy={busy} tabIndex={-1} ref={box}><header className="modal-header"><h2 id="confirm-delete-title">{L('delete')}</h2></header><div className="modal-body"><p id="confirm-delete-message" style={{overflowWrap:'anywhere'}}>{L('confirmDelete'+kind[0].toUpperCase()+kind.slice(1)).replace('{name}',name)}</p>{error&&<div className="callout error" role="alert">{error}</div>}</div><footer className="modal-footer"><button type="button" ref={cancel} disabled={busy} onClick={onClose}>{L('cancel')}</button><button type="button" className="danger" disabled={busy} onClick={()=>void remove()}>{L('delete')}</button></footer></div></div>,document.querySelector('.app')??document.body);
}
