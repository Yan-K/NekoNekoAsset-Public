import {useEffect,useLayoutEffect,useRef,useState} from 'react';
import {createPortal} from 'react-dom';
import type {Locale} from './model';
import {t} from './i18n';
import {errorCode} from './backend';
import {nativeErrorText} from './error-i18n';
import {resetText,resetErrorText} from './settings-reset-i18n';
import './settings-reset.css';

export default function ResetApp({locale,disabled,onReset}:{locale:Locale;disabled:boolean;onReset:(deleteLibrary:boolean)=>Promise<unknown>}){
 const [mode,setMode]=useState<boolean|null>(null);
 const trigger=useRef<HTMLButtonElement|null>(null);
 return <div className="wf-section reset-app-actions"><button disabled={disabled} onClick={event=>{trigger.current=event.currentTarget;setMode(false);}}>{resetText(locale,'reset')}</button><button className="danger" disabled={disabled} onClick={event=>{trigger.current=event.currentTarget;setMode(true);}}>{resetText(locale,'resetDelete')}</button>{mode!==null&&createPortal(<ResetConfirmation key={String(mode)} locale={locale} deleteLibrary={mode} onReset={onReset} onClose={()=>{setMode(null);trigger.current?.focus({preventScroll:true});}}/>,document.querySelector('.app')??document.body)}</div>;
}
export function ResetConfirmation({locale,deleteLibrary,onReset,onClose}:{locale:Locale;deleteLibrary:boolean;onReset:(deleteLibrary:boolean)=>Promise<unknown>;onClose:()=>void}){
 const [seconds,setSeconds]=useState(5),[busy,setBusy]=useState(false),[error,setError]=useState('');
 const deadline=useRef(performance.now()+5000),box=useRef<HTMLDivElement>(null),cancel=useRef<HTMLButtonElement>(null),inFlight=useRef(false),close=useRef(onClose);close.current=onClose;
 const L=(key:string)=>resetText(locale,key),title=L(deleteLibrary?'resetDelete':'reset');
 useEffect(()=>{const timer=setInterval(()=>setSeconds(Math.max(0,Math.ceil((deadline.current-performance.now())/1000))),100);return()=>clearInterval(timer);},[]);
 useEffect(()=>{
  const key=(event:KeyboardEvent)=>{if(event.isComposing||event.keyCode===229)return;if(event.ctrlKey||event.metaKey){event.stopPropagation();if(['f','z','a','+','=','-','add','subtract'].includes(event.key.toLowerCase()))event.preventDefault();}if(event.key==='Escape'){event.preventDefault();event.stopPropagation();if(!inFlight.current)close.current();}if(event.key==='Tab'){const nodes=[...box.current?.querySelectorAll<HTMLButtonElement>('button:not(:disabled)')??[]];if(!nodes.length){event.preventDefault();box.current?.focus();return;}const index=nodes.indexOf(document.activeElement as HTMLButtonElement);if(event.shiftKey&&index<=0){event.preventDefault();nodes.at(-1)?.focus();}else if(!event.shiftKey&&(index<0||index===nodes.length-1)){event.preventDefault();nodes[0]?.focus();}}};
  document.addEventListener('keydown',key,true);return()=>document.removeEventListener('keydown',key,true);
 },[]);
 useLayoutEffect(()=>{if(busy)box.current?.focus();else cancel.current?.focus();},[busy]);
 async function confirm(){if(inFlight.current||performance.now()<deadline.current)return;inFlight.current=true;setBusy(true);setError('');try{await onReset(deleteLibrary);}catch(error){const code=errorCode(error);setError(code==='storage_busy'?L('busy'):resetErrorText(locale,code)??nativeErrorText(locale,code)??t(locale,'operationFailed'));inFlight.current=false;setBusy(false);}}
 return <div className="modal-backdrop reset-app-backdrop" onMouseDown={event=>{if(event.target===event.currentTarget&&!inFlight.current)onClose();}}><div className="modal reset-app-modal" role="alertdialog" aria-modal="true" aria-labelledby="reset-app-title" aria-describedby="reset-app-message reset-app-retained" aria-busy={busy} tabIndex={-1} ref={box}><header className="modal-header"><h2 id="reset-app-title">{title}</h2></header><div className="modal-body"><p id="reset-app-message">{L(deleteLibrary?'deleteMessage':'resetMessage')}</p><p id="reset-app-retained">{L('retained')}</p>{busy&&<p role="status">{L('restarting')}</p>}{error&&<div className="callout error" role="alert">{error}</div>}</div><footer className="modal-footer"><button ref={cancel} disabled={busy} onClick={onClose}>{t(locale,'cancel')}</button><button disabled={busy||seconds>0} className={seconds===0?'danger reset-app-confirm':undefined} onClick={()=>void confirm()}>{seconds>0?L('wait').replace('{seconds}',String(seconds)):title}</button></footer></div></div>;
}
