/** Persist only fields the user changed, preserving preferences imported elsewhere. */
export function changedPreferences(previous:Record<string,unknown>,next:Record<string,unknown>){
 return Object.fromEntries(Object.entries(next).filter(([key,value])=>JSON.stringify(previous[key])!==JSON.stringify(value)));
}

/** One active refresh and one replaceable trailing request; no queued stale work. */
export function coalescedRefresh(task:()=>Promise<void>){
 let running=false,requested=0;
 const waiters:{version:number;resolve:()=>void;reject:(error:unknown)=>void}[]=[];
 const drain=async()=>{
  running=true;
  while(waiters.length){
   const version=requested;let failure:unknown,failed=false;
   try{await task();}catch(error){failure=error;failed=true;}
   for(let index=waiters.length-1;index>=0;index--){const waiter=waiters[index];if(waiter.version<=version){waiters.splice(index,1);if(failed)waiter.reject(failure);else waiter.resolve();}}
  }
  running=false;
 };
 return ()=>new Promise<void>((resolve,reject)=>{waiters.push({version:++requested,resolve,reject});if(!running)void drain();});
}
