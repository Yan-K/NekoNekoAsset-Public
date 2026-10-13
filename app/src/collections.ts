import type {NativeCollection} from './backend';
const strings=(value:unknown):string[]=>Array.isArray(value)?value.filter((entry):entry is string=>typeof entry==='string'):[];
/** Membership for the captured selection; unrelated products never affect the checkbox. */
export function collectionMembership(members:readonly string[],selected:readonly string[]):{checked:boolean;mixed:boolean}{
 const included=new Set(members),checked=selected.length>0&&selected.every(id=>included.has(id));
 return {checked,mixed:!checked&&selected.some(id=>included.has(id))};
}
/** Presentation while an atomic native write is pending; never changes persisted membership. */
export function pendingCollections(rows:readonly NativeCollection[],args:Record<string,unknown>,newId:(name:string)=>string):NativeCollection[]{
 const next=rows.map(row=>({...row,productIds:[...row.productIds]})),op=args.op??args.operation??'create';
 if(op==='create'){next.push({id:String(args.id),name:String(args.name),productIds:strings(args.productIds)});return next;}
 if(op==='delete')return next.filter(row=>row.id!==args.id);
 if(op==='add'||op==='add_members'){
  for(const name of strings(args.names)){if(!next.some(row=>row.name===name))next.push({id:newId(name),name,productIds:[]});}
  const names=new Set(strings(args.names)),ids=new Set(strings(args.collectionIds));
  for(const row of next){if(names.has(row.name)||ids.has(row.id))row.productIds=[...new Set([...row.productIds,...strings(args.ids)])];}
 }else{
  const row=next.find(row=>row.id===args.id);if(row){if(typeof args.name==='string')row.name=args.name;if(op==='remove_members'){const remove=new Set(strings(args.ids));row.productIds=row.productIds.filter(id=>!remove.has(id));}}
 }
 return next;
}

/** Only the selected collection membership can change the product query. */
export function collectionQueryKey(rows:readonly NativeCollection[],name:string):string {
 if(!name)return '';
 const row=rows.find(row=>row.name===name);
 return JSON.stringify(row?[row.id,[...new Set(row.productIds)].sort()]:[null,[]]);
}
/** Ignore transport order and duplicate memberships when reconciling repeated events. */
export function collectionSnapshotKey(rows:readonly NativeCollection[]):string {
 return JSON.stringify(rows.map(row=>[row.id,row.name,[...new Set(row.productIds)].sort()]).sort((a,b)=>String(a[0]).localeCompare(String(b[0]))));
}

export function reconciledCollectionName(before:readonly NativeCollection[],after:readonly NativeCollection[],name:string):string {
 const id=before.find(row=>row.name===name)?.id;
 return id?after.find(row=>row.id===id)?.name??'':name;
}

/** Session-local ordered writes; presentation is always rebased over committed rows. */
export function collectionMutationQueue(
 send:(args:Record<string,unknown>)=>Promise<{collections:NativeCollection[]}>,
 publish:(rows:NativeCollection[],committed:NativeCollection[],busy:boolean)=>void,
 failed:(error:unknown)=>void,
 newId:()=>string,
){
 type Operation={args:Record<string,unknown>;created:Map<string,string>;resolve:()=>void;reject:(error:unknown)=>void};
 let committed:NativeCollection[]=[],running=false;
 const pending:Operation[]=[];
 // Names created by add use stable provisional IDs until the native atomic add returns.
 const presentation=()=>pending.reduce((rows,operation)=>{
  return pendingCollections(rows,operation.args,name=>operation.created.get(name)??'');
 },committed);
 const update=()=>publish(presentation(),committed,pending.length>0);
 const drain=async()=>{
  if(running)return;running=true;
  try{while(pending.length){
   const operation=pending[0];
   try{
    const op=operation.args.op??operation.args.operation??'create';
    if(['create','rename','update'].includes(String(op))&&typeof operation.args.name==='string'&&committed.some(row=>row.name===operation.args.name&&row.id!==operation.args.id))throw new Error('catalogue.already_exists');
    const result=await send(operation.args);
    committed=result.collections;
    pending.shift();
    for(const [name,temporary] of operation.created){
     const actual=committed.find(row=>row.name===name)?.id;
     if(!actual||actual===temporary)continue;
     for(const next of pending){
      if(next.args.id===temporary)next.args.id=actual;
      if(Array.isArray(next.args.collectionIds))next.args.collectionIds=next.args.collectionIds.map(id=>id===temporary?actual:id);
     }
    }
    operation.resolve();
   }catch(error){pending.shift();failed(error);operation.reject(error);}
   update();
  }}finally{running=false;}
 };
 return {
  setCommitted(rows:NativeCollection[]){committed=rows;update();},
  enqueue(input:Record<string,unknown>){
   const args=structuredClone(input),created=new Map<string,string>(),op=args.op??args.operation??'create';
   if(op==='create'&&!args.id)args.id=newId();
   if(op==='add'||op==='add_members'){
    const current=presentation(),ids=strings(args.collectionIds),names:string[]=[];
    for(const raw of strings(args.names)){
     const name=raw.trim(),row=current.find(row=>row.name===name);
     if(row)ids.push(row.id);else if(!created.has(name)){names.push(name);created.set(name,newId());}
    }
    args.names=names;args.collectionIds=[...new Set(ids)];
   }
   const before=presentation();
   if(op==='create'&&before.some(row=>row.name===String(args.name)))throw new Error('catalogue.already_exists');
   if(collectionSnapshotKey(before)===collectionSnapshotKey(pendingCollections(before,args,name=>created.get(name)??'')))return Promise.resolve();
   let resolve!:()=>void,reject!:(error:unknown)=>void;
   const completion=new Promise<void>((done,fail)=>{resolve=done;reject=fail;});
   pending.push({args,created,resolve,reject});update();void drain();return completion;
  },
 };
}
