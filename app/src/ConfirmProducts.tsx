import {useEffect,useLayoutEffect,useRef,useState} from 'react';
import type {Locale} from './model';
import {shellText} from './shell-i18n';
import {t} from './i18n';
import {nativeErrorText} from './error-i18n';
import {errorCode} from './backend';
import {contentLang} from './content-lang';

/** Confirms one action on a captured product selection; Cancel has initial focus. */
export default function ConfirmProducts({locale,title,message,count,names,confirm,danger=false,onConfirm,onClose}:{locale:Locale;title:string;message:string;count:number;names:string[];confirm:string;danger?:boolean;onConfirm:()=>Promise<void>;onClose:()=>void}){
 const L=(key:string)=>shellText(locale,key)??t(locale,key);
 const [busy,setBusy]=useState(false),[error,setError]=useState('');
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
 async function submit(){
  if(inFlight.current)return;inFlight.current=true;setBusy(true);setError('');box.current?.focus();
  try{await onConfirm();onClose();}
  catch(error){setError(nativeErrorText(locale,errorCode(error))??L('operationFailed'));}
  finally{inFlight.current=false;setBusy(false);}
 }
 return <div className="modal-backdrop" onMouseDown={event=>{if(event.target===event.currentTarget&&!inFlight.current)onClose();}}><div className="modal confirm-products" role="alertdialog" aria-modal="true" aria-labelledby="confirm-products-title" aria-describedby="confirm-products-message" aria-busy={busy} tabIndex={-1} ref={box}>
  <header className="modal-header"><h2 id="confirm-products-title">{title}</h2></header>
  <div className="modal-body"><p id="confirm-products-message">{message.replace('{count}',count.toLocaleString(locale))}</p>{names.length>0&&<ul style={{margin:0,paddingLeft:20,overflowWrap:'anywhere'}}>{names.slice(0,8).map((name,index)=><li key={index} lang={contentLang(name)}>{name}</li>)}</ul>}{names.length>0&&count>Math.min(8,names.length)&&<p>+ {(count-Math.min(8,names.length)).toLocaleString(locale)}</p>}{error&&<div className="callout error" role="alert">{error}</div>}</div>
  <footer className="modal-footer"><button type="button" ref={cancel} disabled={busy} onClick={onClose}>{L('cancel')}</button><button type="button" className={danger?'danger':'primary'} disabled={busy} onClick={()=>void submit()}>{busy?L('processing'):confirm}</button></footer>
 </div></div>;
}
