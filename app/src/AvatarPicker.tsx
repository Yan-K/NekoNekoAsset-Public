import {useLayoutEffect,useRef} from 'react';
import type {AvatarOption,AvatarDelta} from './backend';
import type {Locale,Product} from './model';
import {getAvatarAliases,normalizeSearch} from './model';
import {editorText} from './editor-i18n';
import {shellText} from './shell-i18n';
import AvatarManager from './AvatarManager';

type AvatarProduct=Pick<Product,'ownedAvatars'|'ownedAvatarEvidence'>;
export interface ProductAvatarOption extends AvatarOption {legacyValue?:string}
function persistedAvatarValue(option:ProductAvatarOption):string{
 if(option.legacyValue!==undefined)return option.legacyValue;
 if(!option.id.startsWith('avatar:'))return option.id;
 if(!option.sourceIdentityValue?.trim())throw new Error('catalogue.invalid_avatar_selection');
 return option.sourceIdentityValue;
}
const optionNames=(option:AvatarOption)=>[option.id,option.officialName,option.englishName,...option.aliases].filter((name):name is string=>!!name);
function matchingOptions(value:string,options:ProductAvatarOption[]):ProductAvatarOption[]{
 const aliases=new Set(getAvatarAliases(value).map(normalizeSearch));
 return options.filter(option=>option.legacyValue===undefined&&option.nameKind!=='bundle'&&option.nameKind!=='fallback'&&optionNames(option).some(name=>getAvatarAliases(name).some(alias=>aliases.has(normalizeSearch(alias)))));
}
function avatarIdentities(product:AvatarProduct,value:string,options:ProductAvatarOption[]):string[]{
 const direct=options.find(option=>option.id===value);if(direct)return [direct.id];
 const evidence=new Set((product.ownedAvatarEvidence??[]).filter(item=>normalizeSearch(item.avatar)===normalizeSearch(value)&&options.some(option=>option.id===item.avatarId)).map(item=>item.avatarId!));
 if(evidence.size)return [...evidence];
 const legacy=options.find(option=>option.legacyValue===value);if(legacy)return [legacy.id];
 const matches=matchingOptions(value,options);return matches.length===1?[matches[0].id]:[];
}
export function productsAvatarOptions(products:AvatarProduct[],options:AvatarOption[]):ProductAvatarOption[]{
 const existing=[...new Set(products.flatMap(product=>product.ownedAvatars.filter(value=>!avatarIdentities(product,value,options).length)))];
 return [...options,...existing.map((value,index)=>({id:`editor-existing:${index}`,officialName:value,displayName:value,aliases:[],legacyValue:value,matchingProductCount:products.filter(product=>product.ownedAvatars.includes(value)).length}))];
}
export function productAvatarOptions(product:AvatarProduct,options:AvatarOption[]):ProductAvatarOption[]{return productsAvatarOptions([product],options);}
export function productAvatarDelta(options:ProductAvatarOption[],initial:readonly string[],selected:readonly string[]):AvatarDelta{
 const value=(id:string)=>options.find(option=>option.id===id)?.legacyValue??id;
 return {add:selected.filter(id=>!initial.includes(id)).map(value),remove:initial.filter(id=>!selected.includes(id)).map(value)};
}
export function productAvatarSelection(product:AvatarProduct,options:ProductAvatarOption[]):string[]{
 return [...new Set(product.ownedAvatars.flatMap(value=>avatarIdentities(product,value,options)))];
}
export function productAvatarValues(product:AvatarProduct,options:ProductAvatarOption[],selected:readonly string[],openingOptions=options):string[]{
 const values:string[]=[],retained=new Set<string>();
 for(const value of product.ownedAvatars){
  const ids=avatarIdentities(product,value,openingOptions);
  if(!ids.length){values.push(value);continue;}
  for(const id of ids){
   if(!selected.includes(id))continue;
   // A shared friendly label cannot retain one of several exact identities.
   const option=options.find(option=>option.id===id)??openingOptions.find(option=>option.id===id);if(!option)continue;
   const legacy=option.legacyValue;
   const ambiguous=legacy===undefined&&(ids.length>1||matchingOptions(value,openingOptions).length>1);
   values.push(ambiguous||value===id?persistedAvatarValue(option):value);retained.add(id);
  }
 }
 for(const id of selected){
  if(retained.has(id))continue;
  const option=options.find(option=>option.id===id);if(!option)continue;
  values.push(persistedAvatarValue(option));retained.add(id);
 }
 return [...new Set(values)];
}

export default function AvatarPicker({locale,options,value,mixed,label,onChange,onClose}:{locale:Locale;options:AvatarOption[];value:string[];mixed?:(id:string)=>boolean;label:(id:string)=>string;onChange:(ids:string[],selected:boolean)=>void;onClose:()=>void}){
 const box=useRef<HTMLDivElement>(null),close=useRef(onClose);close.current=onClose;
 const L=(key:string)=>editorText(locale,key);
 useLayoutEffect(()=>{
  const prior=document.activeElement as HTMLElement|null;
  const focusable=()=>Array.from(box.current?.querySelectorAll<HTMLElement>('button:not(:disabled),input:not(:disabled),summary')??[]).filter(element=>element.offsetParent!==null);
  (box.current?.querySelector<HTMLInputElement>('input')??focusable()[0])?.focus();
  const key=(event:KeyboardEvent)=>{
   if(event.isComposing||event.keyCode===229)return;
   if(event.key==='Escape'){event.preventDefault();event.stopPropagation();close.current();}
   if(event.key==='Tab'){
    const elements=focusable(),first=elements[0],last=elements.at(-1);
    if(event.shiftKey&&(document.activeElement===first||!elements.includes(document.activeElement as HTMLElement))){event.preventDefault();last?.focus();}
    else if(!event.shiftKey&&(document.activeElement===last||!elements.includes(document.activeElement as HTMLElement))){event.preventDefault();first?.focus();}
   }
  };
  document.addEventListener('keydown',key,true);
  return()=>{document.removeEventListener('keydown',key,true);if(prior?.isConnected)prior.focus();};
 },[]);
 return <div className="modal-backdrop editor-avatar-backdrop" onMouseDown={event=>{if(event.target===event.currentTarget)onClose();}}><div className="modal editor-avatar-picker avatar-dialog" role="dialog" aria-modal="true" aria-labelledby="editor-avatar-title" ref={box}>
  <header className="modal-header"><h2 id="editor-avatar-title">{L('compatibleAvatars')}</h2><button type="button" aria-label={L('close')} onClick={onClose}>×</button></header>
  <div className="modal-body"><AvatarManager options={options} enabled={id=>value.includes(id)} mixed={mixed} label={label} onChange={onChange} q={key=>key==='unknownShop'?(shellText(locale,key)??L(key)):L(key==='allAvatarsOn'?'selectAllAvatars':key)}/></div>
  <footer className="modal-footer"><button type="button" onClick={onClose}>{L('close')}</button></footer>
 </div></div>;
}
