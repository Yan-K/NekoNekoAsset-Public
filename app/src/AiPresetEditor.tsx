import {useEffect,useRef,useState} from 'react';
import {Download,Upload,X} from 'lucide-react';
import type {Locale} from './model';
import {dialog,errorCode,exportAiPreset,importAiPreset,nativeFileAction,saveAiPreset,type AiCustomPreset} from './backend';
import {aiSuggestionsText} from './ai-suggestions-i18n';

export default function AiPresetEditor({locale,preset,onSaved,onClose}:{locale:Locale;preset:AiCustomPreset|null;onSaved:(preset:AiCustomPreset)=>void;onClose:()=>void}) {
 const L=(key:string)=>aiSuggestionsText(locale,key);
 const [title,setTitle]=useState(preset?.title??'');
 const [description,setDescription]=useState(preset?.description??'');
 const [systemPrompt,setSystemPrompt]=useState(preset?.systemPrompt??'');
 const [saved,setSaved]=useState<AiCustomPreset|null>(preset);
 const [busy,setBusy]=useState(false);
 const [error,setError]=useState('');
 const lock=useRef(false);
 const mounted=useRef(true);
 const titleInput=useRef<HTMLInputElement>(null);
 useEffect(()=>{mounted.current=true;const previous=document.activeElement as HTMLElement|null;titleInput.current?.focus();return()=>{mounted.current=false;previous?.focus();};},[]);
 const valid=!!title.trim()&&!!systemPrompt.trim()&&!/[\u0000-\u001f\u007f]/.test(title)&&new TextEncoder().encode(systemPrompt).length<=16000;
 const dirty=!saved||title!==saved.title||description!==saved.description||systemPrompt!==saved.systemPrompt;
 async function save(){
  if(lock.current||!valid)return;
  lock.current=true;setBusy(true);setError('');
  try{const value=await saveAiPreset({...(saved?{id:saved.id}:{}),title:title.trim(),description:description.trim(),systemPrompt:systemPrompt.trim()});if(mounted.current){setSaved(value);setTitle(value.title);setDescription(value.description);setSystemPrompt(value.systemPrompt);onSaved(value);}}
  catch(value){if(mounted.current)setError(errorCode(value));}
  finally{lock.current=false;if(mounted.current)setBusy(false);}
 }
 async function importPreset(){
  if(lock.current)return;
  lock.current=true;setBusy(true);setError('');
  try{
   const chosen=await dialog('open_file',{filters:[{name:'JSON',extensions:['json']}]});if(!chosen)return;
   const file=await nativeFileAction<{text?:string;content?:string}>('read_text',{path:chosen.path});
   const text=file.text??file.content;if(typeof text!=='string'||new TextEncoder().encode(text).length>65536)throw new Error('ai_preset_invalid');
   const value=await importAiPreset(text);
   if(mounted.current){setSaved(value);setTitle(value.title);setDescription(value.description);setSystemPrompt(value.systemPrompt);onSaved(value);}
  }catch(value){if(mounted.current)setError(errorCode(value));}
  finally{lock.current=false;if(mounted.current)setBusy(false);}
 }
 async function exportPreset(){
  if(lock.current||!saved||dirty)return;
  lock.current=true;setBusy(true);setError('');
  try{
   const chosen=await dialog('save_file',{defaultName:'NekoNekoAsset-AI-preset.json',filters:[{name:'JSON',extensions:['json']}]});if(!chosen)return;
   const file=await exportAiPreset(saved.id);await nativeFileAction('write_text',{path:chosen.path,text:file.json});
  }catch(value){if(mounted.current)setError(errorCode(value));}
  finally{lock.current=false;if(mounted.current)setBusy(false);}
 }
 return <div className="ai-preset-editor-backdrop" onKeyDown={event=>{
  if(event.key==='Escape'&&!busy){event.stopPropagation();onClose();}
  if(event.key==='Tab'){const fields=Array.from(event.currentTarget.querySelectorAll<HTMLElement>('button:not(:disabled),input:not(:disabled),textarea:not(:disabled)'));const first=fields[0],last=fields.at(-1);if(event.shiftKey&&document.activeElement===first){event.preventDefault();last?.focus();}else if(!event.shiftKey&&document.activeElement===last){event.preventDefault();first?.focus();}}
 }}>
  <section className="ai-preset-editor" role="dialog" aria-modal="true" aria-labelledby="ai-preset-editor-title">
   <header><h2 id="ai-preset-editor-title">{L(saved?'editPreset':'addPreset')}</h2><button type="button" className="icon-button" disabled={busy} aria-label={L('close')} onClick={onClose}><X size={16}/></button></header>
   <div className="ai-preset-editor-fields">
    <label>{L('presetTitle')}<input ref={titleInput} value={title} maxLength={120} disabled={busy} onChange={event=>setTitle(event.target.value)}/></label>
    <label>{L('presetDescription')}<textarea value={description} maxLength={1000} disabled={busy} onChange={event=>setDescription(event.target.value)}/></label>
    <label>{L('systemPrompt')}<textarea className="ai-preset-system-prompt" value={systemPrompt} maxLength={16000} disabled={busy} aria-invalid={new TextEncoder().encode(systemPrompt).length>16000} onChange={event=>setSystemPrompt(event.target.value)}/></label>
    {new TextEncoder().encode(systemPrompt).length>16000&&<div className="ai-chat-error" role="alert">{L('ai_preset_prompt_too_long')}</div>}
    {error&&<div className="ai-chat-error" role="alert">{L(error)}</div>}
   </div>
   <footer><div><button type="button" disabled={busy} onClick={()=>void importPreset()}><Upload size={14}/>{L('importPreset')}</button><button type="button" disabled={busy||!saved||dirty} title={dirty?L('saveBeforeExport'):undefined} onClick={()=>void exportPreset()}><Download size={14}/>{L('exportPreset')}</button></div><div><button type="button" disabled={busy} onClick={onClose}>{L('close')}</button><button type="button" className="primary" disabled={busy||!valid||!dirty} onClick={()=>void save()}>{L('save')}</button></div></footer>
  </section>
 </div>;
}
