import {useEffect,useRef} from 'react';
import type {AvatarOption} from './backend';

function GroupCheckbox({checked,mixed,label,onChange}:{checked:boolean;mixed:boolean;label:string;onChange:(enabled:boolean)=>void}){
 const ref=useRef<HTMLInputElement>(null);useEffect(()=>{if(ref.current)ref.current.indeterminate=mixed;},[mixed]);
 return <input ref={ref} type="checkbox" checked={checked} aria-label={label} onClick={event=>event.stopPropagation()} onKeyDown={event=>{if(event.key===' '||event.key==='Enter')event.stopPropagation();}} onChange={event=>onChange(event.target.checked)}/>;
}

/** Canonical native identities only; visibility is independent of account ownership. */
export default function AvatarManager({options,enabled,label,onChange,q}:{options:AvatarOption[];enabled:(id:string)=>boolean;label:(id:string)=>string;onChange:(ids:string[],enabled:boolean)=>void;q:(key:string)=>string}){
 const groups=new Map<string,AvatarOption[]>();for(const option of options){const shop=option.shopName?.trim()||q('unknownShop');groups.set(shop,[...(groups.get(shop)??[]),option]);}
 const allOn=options.length>0&&options.every(option=>enabled(option.id)),allOff=options.length>0&&options.every(option=>!enabled(option.id));
 return <div className="avatar-manager-tree">
  <div className="avatar-manager-all"><label className="check-row"><input type="checkbox" checked={allOn} onChange={event=>onChange(options.map(option=>option.id),event.target.checked)}/>{q('allAvatarsOn')}</label><label className="check-row"><input type="checkbox" checked={allOff} onChange={event=>onChange(options.map(option=>option.id),!event.target.checked)}/>{q('allAvatarsOff')}</label></div>
  {[...groups].map(([shop,avatars])=>{const count=avatars.filter(option=>enabled(option.id)).length;return <details key={shop} className="avatar-shop-group">
   <summary><GroupCheckbox checked={count===avatars.length} mixed={count>0&&count<avatars.length} label={shop} onChange={value=>onChange(avatars.map(option=>option.id),value)}/><span title={shop}>{shop}</span><small>{count}/{avatars.length}</small></summary>
   <div className="avatar-shop-options">{avatars.map(option=><label className="check-row" key={option.id}><input type="checkbox" checked={enabled(option.id)} onChange={event=>onChange([option.id],event.target.checked)}/><span title={label(option.id)}>{label(option.id)}</span><small>{option.matchingProductCount??0}</small></label>)}</div>
  </details>;})}
 </div>;
}
