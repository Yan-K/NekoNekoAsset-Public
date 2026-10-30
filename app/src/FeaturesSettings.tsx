import {useState} from 'react';
import type {Locale,SensitiveMode} from './model';
import {t} from './i18n';
import {uxText} from './ux-i18n';
import {shellText} from './shell-i18n';
import Toggle from './Toggle';
import './features.css';

export const DEFAULT_BATCH_ACTIONS:Record<string,boolean>={edit:true,collection:true,unity:true,export:true,ai:true,spending:true,delete:true,hide:true,selectAll:true,update:true,download:true};
export function batchActionPreferences(value:unknown):Record<string,boolean>{return {...DEFAULT_BATCH_ACTIONS,...(value&&typeof value==='object'&&!Array.isArray(value)?Object.fromEntries(Object.entries(value).filter(([key,item])=>key in DEFAULT_BATCH_ACTIONS&&typeof item==='boolean')):{})};}
const actions=[['edit','edit'],['collection','addToCollection'],['unity','importUnity'],['export','exportZip'],['ai','aiTags'],['spending','spending'],['delete','delete'],['hide','hide'],['selectAll','selectAll'],['update','update'],['download','download']];
export default function FeaturesSettings({locale,preferences,onPreferences}:{locale:Locale;preferences:Record<string,unknown>;onPreferences:(patch:Record<string,unknown>)=>Promise<void>}){
 const q=(key:string)=>uxText(locale,key)||shellText(locale,key)||t(locale,key),[busy,setBusy]=useState(false),[error,setError]=useState('');
 const batch=batchActionPreferences(preferences.batchActions);
 const save=async(patch:Record<string,unknown>)=>{setBusy(true);setError('');try{await onPreferences(patch);return true;}catch{setError(t(locale,'operationFailed'));return false;}finally{setBusy(false);}};
 return <div className="workflow-content feature-settings">
  <section className="wf-section"><h3>R-18</h3><div className="segmented sensitive-modes" role="group" aria-label="R-18">{(['blur','hide','show','only'] as SensitiveMode[]).map(mode=>{const active=String(preferences.sensitiveMode||'blur')===mode,label=mode==='only'?t(locale,'r18Only'):t(locale,mode);return <button key={mode} type="button" title={label} aria-pressed={active} disabled={busy} onClick={()=>{if(!active)void save({sensitiveMode:mode});}}><span>{label}</span></button>;})}</div></section>
  <section className="wf-section"><h3>AI</h3><Toggle label={q('aiFeatures')} checked={preferences.aiFeaturesEnabled===true} disabled={busy} onChange={enabled=>void save({aiFeaturesEnabled:enabled})}/></section>
  <section className="wf-section"><h3>{q('batchActions')}</h3><div className="feature-actions">{actions.map(([id,label])=><label key={id} className={id==='ai'&&preferences.aiFeaturesEnabled!==true?'feature-disabled':undefined}><input type="checkbox" checked={batch[id]} disabled={busy||id==='ai'&&preferences.aiFeaturesEnabled!==true} onChange={event=>void save({batchActions:{...batch,[id]:event.target.checked}})}/><span>{q(label)}</span></label>)}</div>
  </section>{error&&<small role="status" className="feature-error">{error}</small>}
 </div>;
}
