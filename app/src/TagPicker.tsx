import { useId, useMemo, useRef, useState } from 'react';
import type { Locale } from './model';
import { normalizeSearch, getAvatarAliases } from './model';
import { editorText } from './editor-i18n';
import './editor-styles.css';

export interface TagPickerProps {
  locale: Locale;
  value: string[];
  suggestions: string[];
  onChange: (tags: string[]) => void;
  onRemove?: (tag: string) => void;
  onDraftChange?: (text: string) => void;
  aliases?: Record<string, readonly string[]>;
  label?: string;
  initialQuery?: string;
}
/** Resolve an explicitly entered existing alias without changing the displayed source tag. */
export function resolveTag(text: string, suggestions: readonly string[], aliases: Record<string, readonly string[]> = {}): string {
  const query = normalizeSearch(text);
  return [...new Set([...suggestions,...Object.keys(aliases)])].find(tag => normalizeSearch(tag) === query || (aliases[tag] ?? getAvatarAliases(tag)).some(alias => normalizeSearch(alias) === query)) ?? text.trim();
}
export default function TagPicker({locale,value,suggestions,onChange,onRemove,onDraftChange,aliases={},label,initialQuery=''}: TagPickerProps) {
  const L = (key: string) => editorText(locale,key);
  const id = useId();
  const [query,setQuery] = useState(initialQuery);
  const [choice,setChoice] = useState(-1);
  const input = useRef<HTMLInputElement>(null);
  const composing = useRef(false);
  const options = useMemo(() => !query.trim() ? [] : [...new Set(suggestions)].filter(tag => !value.some(current=>normalizeSearch(current)===normalizeSearch(tag)) && (!query.trim() || normalizeSearch([tag,...(aliases[tag]??getAvatarAliases(tag))].join(' ')).includes(normalizeSearch(query)))).slice(0,8),[suggestions,value,query,aliases]);
  const canonical = resolveTag(query,suggestions,aliases);
  const canAdd = !!canonical && !value.some(tag=>normalizeSearch(tag)===normalizeSearch(canonical));
  const existing = suggestions.includes(canonical) || Object.hasOwn(aliases,canonical);
  const changeQuery = (text:string) => { setQuery(text); setChoice(-1); onDraftChange?.(text); };
  const add = (tag:string) => { const clean = tag.trim(); if(clean && !value.some(current=>normalizeSearch(current)===normalizeSearch(clean))) onChange([...value,clean]); changeQuery(''); input.current?.focus(); };
  const remove = (tag:string) => { onRemove?.(tag); onChange(value.filter(current=>current!==tag)); input.current?.focus(); };
  return <div className="editor-tag-picker field">
    <label htmlFor={id}>{label ?? L('tags')}</label>
    <div className="editor-tag-entry">
      <input id={id} ref={input} value={query} placeholder={L('tagInput')} autoComplete="off" role="combobox" aria-autocomplete="list" aria-expanded={!!query.trim()&&options.length>0} aria-controls={`${id}-options`} aria-activedescendant={choice>=0&&options[choice]?`${id}-option-${choice}`:undefined}
        onChange={e=>changeQuery(e.target.value)}
        onCompositionStart={()=>{composing.current=true;}} onCompositionEnd={()=>{composing.current=false;}}
        onKeyDown={e=>{
          if(composing.current || e.nativeEvent.isComposing || e.keyCode===229) return;
          if(e.key==='ArrowDown' && options.length) { e.preventDefault(); setChoice(index=>(index+1)%options.length); }
          if(e.key==='ArrowUp' && options.length) { e.preventDefault(); setChoice(index=>(index-1+options.length)%options.length); }
          if(e.key==='Enter') { e.preventDefault(); if(choice>=0&&options[choice]) add(options[choice]); else if(canAdd) add(canonical); }
          if(e.key==='Backspace' && !query && value.length) { e.preventDefault(); remove(value[value.length-1]); }
        }}/>
      <button type="button" disabled={!canAdd} onClick={()=>add(canonical)} title={existing?L('addTagChoice'):L('createTag')} aria-label={`${existing?L('addTagChoice'):L('createTag')}${canonical?`: ${canonical}`:''}`}>+</button>
    </div>
    {!!query.trim() && options.length>0 && <div className="editor-tag-options" id={`${id}-options`} role="listbox" aria-label={L('tagSuggestions')}>{options.map((tag,index)=><button type="button" id={`${id}-option-${index}`} key={tag} role="option" aria-selected={index===choice} onMouseDown={e=>e.preventDefault()} onClick={()=>add(tag)}>{tag}</button>)}</div>}
    {value.length>0 && <div className="editor-tag-chips">{value.map(tag=><button type="button" key={tag} onMouseDown={e=>e.preventDefault()} onClick={()=>remove(tag)} aria-label={`${L('removeTagChoice')}: ${tag}`} title={tag}><span>{tag}</span><span aria-hidden="true">×</span></button>)}</div>}
  </div>;
}




