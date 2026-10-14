// One bounded private JSON exchange. No HTTP or client/SDK imports.
const {createLoginMaterial,createRefreshProof}=require('./crypto.cjs');
const LIMIT=64*1024;let size=0;const chunks=[];
function fail(){process.stderr.write('TRAE_PRIVATE_PROTOCOL_FAILED\n');process.exitCode=1;}
process.stdin.on('data',chunk=>{size+=chunk.length;if(size>LIMIT){process.stdin.destroy();fail();return;}chunks.push(chunk);});
process.stdin.on('error',()=>fail());
process.stdin.on('end',()=>{
  if(process.exitCode)return;
  try{
    const input=JSON.parse(new TextDecoder('utf-8',{fatal:true}).decode(Buffer.concat(chunks)));
    if(!input||typeof input!=='object'||Array.isArray(input))throw Error('invalid envelope');
    let result;
    if(input.operation==='login_material'&&Object.keys(input).length===1)result=createLoginMaterial();
    else if(input.operation==='refresh_proof'&&Object.keys(input).length===2&&Object.hasOwn(input,'input'))result=createRefreshProof(input.input);
    else throw Error('unverified operation');
    process.stdout.write(JSON.stringify(result));
  }catch{fail();}
});
