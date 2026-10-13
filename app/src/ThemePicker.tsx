import type { CSSProperties } from 'react';
import type { Locale } from './model';
import { wf } from './workflow-i18n';
import './theme-picker.css';

export type ThemeOption = { id:string; name:string; tokens:Record<string,string> };
export type ThemePickerProps = { locale:Locale; options:ThemeOption[]; value:string; onChange:(id:string)=>void };
export const defaultThemeOptions:ThemeOption[] = [
 {id:'dark',name:'Dark',tokens:{'--bg':'#171717','--panel':'#202020','--raised':'#292929','--border':'#373737','--text':'#ededed','--muted':'#a1a1a1','--accent':'#b6daca','--secondary-accent':'#8bb8f0','--secondary-accent-text':'#171717','--control':'#303030','--positive':'#b6daca','--warning':'#dfc689','--negative':'#e49c9c'}},
 {id:'light',name:'Light',tokens:{'--bg':'#f1f1f1','--panel':'#fafafa','--raised':'#ffffff','--border':'#d5d5d5','--text':'#252525','--muted':'#696969','--accent':'#426f5d','--secondary-accent':'#356cae','--secondary-accent-text':'#ffffff','--control':'#e9e9e9','--positive':'#386d55','--warning':'#896514','--negative':'#ad4343'}}
];
function previewTokens(tokens:Record<string,string>):CSSProperties {
 const defaults=defaultThemeOptions[0].tokens;
 return Object.fromEntries(['bg','panel','raised','border','text','accent'].map(key=>{
  const value=tokens['--'+key]??tokens[key];
  return ['--preview-'+key,typeof value==='string'&&/^#[a-f0-9]{6}$/i.test(value)?value:defaults['--'+key]];
 })) as CSSProperties;
}
export default function ThemePicker({locale,options,value,onChange}:ThemePickerProps) {
 return <div className="theme-picker" role="radiogroup" aria-label={wf(locale,'theme')}>
  {options.map((option,index)=>{const name=['dark','light'].includes(option.id)?wf(locale,option.id):option.name;return <button type="button" key={option.id} className="nna-theme-option" role="radio" aria-checked={value===option.id} aria-label={name} title={name} tabIndex={value===option.id||(!options.some(item=>item.id===value)&&index===0)?0:-1} onClick={()=>onChange(option.id)} onKeyDown={event=>{const offset=event.key==='ArrowRight'||event.key==='ArrowDown'?1:event.key==='ArrowLeft'||event.key==='ArrowUp'?-1:0;if(!offset&&event.key!=='Home'&&event.key!=='End')return;event.preventDefault();const next=event.key==='Home'?0:event.key==='End'?options.length-1:(index+offset+options.length)%options.length;onChange(options[next].id);event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>('[role="radio"]')[next]?.focus();}}>
   <span className="theme-miniature" style={previewTokens(option.tokens)} aria-hidden="true"><span className="theme-mini-top"><i/><b/></span><span className="theme-mini-body"><span className="theme-mini-nav"><i/><i/><i/></span><span className="theme-mini-main"><i/><i/><i/></span></span></span>
   <span className="theme-option-name">{name}</span><span className="theme-option-check" aria-hidden="true">{value===option.id?'✓':''}</span>
  </button>;})}
 </div>;
}
