import {useEffect,useRef,useState} from 'react';
import {Check,Pencil,Search,X} from 'lucide-react';
import type {AvatarOption} from './backend';
import {normalizeSearch} from './model';

function AvatarCheckbox({checked,mixed,label,disabled=false,onChange}:{checked:boolean;mixed:boolean;label:string;disabled?:boolean;onChange:(enabled:boolean)=>void}){
 const ref=useRef<HTMLInputElement>(null);useEffect(()=>{if(ref.current)ref.current.indeterminate=mixed;},[mixed]);
 return <input ref={ref} type="checkbox" checked={checked} aria-checked={mixed?'mixed':checked} aria-label={label} disabled={disabled} onClick={event=>event.stopPropagation()} onKeyDown={event=>{if(event.key===' '||event.key==='Enter')event.stopPropagation();}} onChange={event=>onChange(event.target.checked)}/>;
}
export interface AvatarManagerProps {
 options:AvatarOption[];enabled:(id:string)=>boolean;mixed?:(id:string)=>boolean;label:(id:string)=>string;
 onChange:(ids:string[],enabled:boolean)=>void;q:(key:string)=>string;
 onEdit?:(id:string,patch:{displayName:string;shopName:string})=>void|Promise<void>;
}
type AvatarNameDraft={id:string;displayName:string;shopName:string;sourceDisplayName:string;sourceShopName:string};

/** The same grouped identities serve global visibility and product compatibility drafts. */
export default function AvatarManager({options,enabled,mixed=()=>false,label,onChange,q,onEdit}:AvatarManagerProps){
 const [search,setSearch]=useState(''),[expanded,setExpanded]=useState<Set<string>>(()=>new Set()),[editing,setEditing]=useState<AvatarNameDraft|null>(null),[saving,setSaving]=useState(false),[error,setError]=useState('');
 const tree=useRef<HTMLDivElement>(null),editName=useRef<HTMLInputElement>(null),lastEdit=useRef<string|null>(null),alive=useRef(true),inFlight=useRef(false);
 useEffect(()=>{alive.current=true;return()=>{alive.current=false;};},[]);
 useEffect(()=>{if(editing){lastEdit.current=editing.id;editName.current?.focus();}else if(lastEdit.current){const id=lastEdit.current;lastEdit.current=null;const trigger=Array.from(tree.current?.querySelectorAll<HTMLButtonElement>('[data-avatar-edit]')??[]).find(button=>button.dataset.avatarEdit===id&&button.getClientRects().length>0&&!button.closest('details:not([open])'));(trigger??tree.current?.querySelector<HTMLInputElement>('.avatar-manager-search input'))?.focus();}},[editing?.id]);
 const query=normalizeSearch(search);
 const groups=new Map<string,AvatarOption[]>();
 for(const option of options){
  const shop=option.shopName?.trim()||q('unknownShop');
  const names=[shop,option.sourceShopName,label(option.id),option.sourceDisplayName,option.officialName,option.englishName,...option.aliases];
  if(query&&!names.some(name=>name&&normalizeSearch(name).includes(query)))continue;
  groups.set(shop,[...(groups.get(shop)??[]),option]);
 }
 const visible=[...groups.values()].flat(),allOn=visible.length>0&&visible.every(option=>enabled(option.id)&&!mixed(option.id)),allMixed=!allOn&&visible.some(option=>enabled(option.id)||mixed(option.id));
 const startEdit=(option:AvatarOption)=>{
  if(inFlight.current)return;
  const sourceDisplayName=option.sourceDisplayName||[option.officialName,option.englishName].filter(Boolean).join(' / '),sourceShopName=option.sourceShopName||'';
  setEditing({id:option.id,displayName:option.displayName===sourceDisplayName?'':option.displayName||'',shopName:(option.shopName||'')===sourceShopName?'':option.shopName||'',sourceDisplayName,sourceShopName});setError('');
 };
 const cancelEdit=()=>{if(inFlight.current)return;setEditing(null);setError('');};
 const saveEdit=async()=>{
  if(!editing||!onEdit||inFlight.current)return;
  const draft=editing;inFlight.current=true;setSaving(true);setError('');
  try{await onEdit(draft.id,{displayName:draft.displayName.trim(),shopName:draft.shopName.trim()});if(alive.current)setEditing(null);}
  catch{if(alive.current)setError(q('avatarEditFailed'));}
  finally{inFlight.current=false;if(alive.current)setSaving(false);}
 };
 return <div className="avatar-manager-tree" ref={tree}>
  <div className="avatar-manager-search"><Search size={14} aria-hidden="true"/><input type="search" aria-label={q('avatarSearch')} placeholder={q('avatarSearch')} value={search} disabled={saving} onChange={event=>setSearch(event.target.value)}/></div>
  <div className="avatar-manager-all"><label className="check-row"><AvatarCheckbox checked={allOn} mixed={allMixed} disabled={saving||!visible.length} label={q('allAvatarsOn')} onChange={value=>onChange(visible.map(option=>option.id),value)}/>{q('allAvatarsOn')}</label></div>
  <div className="avatar-manager-scroll">
   {!visible.length&&<div className="avatar-manager-empty">{q('avatarNoResults')}</div>}
   {[...groups].map(([shop,avatars])=>{
    const count=avatars.filter(option=>enabled(option.id)&&!mixed(option.id)).length,groupMixed=count!==avatars.length&&avatars.some(option=>enabled(option.id)||mixed(option.id));
    return <details key={shop} className="avatar-shop-group" open={!!query||expanded.has(shop)} onToggle={event=>{if(query)return;const open=event.currentTarget.open;setExpanded(current=>{if(current.has(shop)===open)return current;const next=new Set(current);if(open)next.add(shop);else next.delete(shop);return next;});}}>
     <summary><AvatarCheckbox checked={count===avatars.length} mixed={groupMixed} disabled={saving} label={shop} onChange={value=>onChange(avatars.map(option=>option.id),value)}/><span title={shop}>{shop}</span><small>{count}/{avatars.length}</small></summary>
     <div className="avatar-shop-options">{avatars.map(option=><div className="avatar-option" key={option.id}>
      <div className="avatar-option-row"><label className="check-row"><AvatarCheckbox checked={enabled(option.id)} mixed={mixed(option.id)} disabled={saving} label={label(option.id)} onChange={value=>onChange([option.id],value)}/><span title={label(option.id)}>{label(option.id)}</span><small>{option.matchingProductCount??0}</small></label>{onEdit&&<button type="button" className="avatar-edit-trigger" data-avatar-edit={option.id} aria-label={q('editAvatar')+' '+label(option.id)} title={q('editAvatar')} disabled={saving} onClick={()=>startEdit(option)}><Pencil size={12}/></button>}</div>
      {editing?.id===option.id&&<div className="avatar-inline-editor" onKeyDown={event=>{if(event.nativeEvent.isComposing||event.keyCode===229)return;if(event.key==='Escape'){event.preventDefault();event.stopPropagation();cancelEdit();}else if(event.key==='Enter'&&event.target instanceof HTMLInputElement){event.preventDefault();event.stopPropagation();void saveEdit();}}}>
       <label><span>{q('avatarName')}</span><input ref={editName} value={editing.displayName} placeholder={editing.sourceDisplayName} disabled={saving} onChange={event=>setEditing({...editing,displayName:event.target.value})}/></label>
       <label><span>{q('avatarShop')}</span><input value={editing.shopName} placeholder={editing.sourceShopName||q('unknownShop')} disabled={saving} onChange={event=>setEditing({...editing,shopName:event.target.value})}/></label>
       <div className="avatar-inline-actions"><button type="button" disabled={saving} aria-label={q('save')} title={q('save')} aria-busy={saving} onClick={()=>void saveEdit()}><Check size={14}/></button><button type="button" disabled={saving} aria-label={q('cancel')} title={q('cancel')} onClick={cancelEdit}><X size={14}/></button></div>
       {error&&<small className="avatar-inline-error" role="alert">{error}</small>}
      </div>}
     </div>)}</div>
    </details>;
   })}
  </div>
 </div>;
}
