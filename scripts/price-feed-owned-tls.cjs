// Inert owned HTTPS fixture. Ephemeral PFX arrives only through an anonymous pipe.
// No credential files, external destinations, model programs or trust changes.
const https=require('node:https'),http=require('node:http'),readline=require('node:readline'),zlib=require('node:zlib');
const input=readline.createInterface({input:process.stdin,crlfDelay:Infinity});
input.once('line',async line=>{
 input.close();process.stdin.pause();process.stdin.unref?.();let origin,control;const sockets=new Set();
 try{
  const owned=JSON.parse(line),max=owned.max_bytes;
  if(!['normal','exact','over_hold','gzip_over','status_hold'].includes(owned.mode)||!Number.isInteger(max)||max<1024||max>32*1024*1024)throw Error('owned bounded fixture mode/size required');
  const state={requests:0,wire_offered:0,decoded_fixture_bytes:0,flushed:false,response_closed:0,authorization_headers:0,error:''};
  const json=Buffer.from(JSON.stringify({data:[{id:'owned-price-model',pricing:{prompt:'0.000001',completion:'0.000002'}}]}));
  origin=https.createServer({pfx:Buffer.from(owned.pfx,'base64'),passphrase:owned.password},(req,res)=>{
   if(req.method!=='GET'||req.url!=='/feed'){state.error='unexpected owned feed route';res.writeHead(404);res.end();return;}
   state.requests++;if(req.headers.authorization)state.authorization_headers++;res.once('close',()=>state.response_closed++);
   if(owned.mode==='status_hold'){res.writeHead(503,{'content-type':'application/json'});res.flushHeaders();res.write('owned error body remains unfinished',()=>{state.flushed=true;});return;}
   let payload=json;
   if(owned.mode==='exact')payload=Buffer.concat([json,Buffer.alloc(max-json.length,32)]);
   if(['over_hold','gzip_over'].includes(owned.mode))payload=Buffer.concat([json,Buffer.alloc(max+1-json.length,32)]);
   state.decoded_fixture_bytes=payload.length;
   if(owned.mode==='gzip_over'){payload=zlib.gzipSync(payload);res.setHeader('content-encoding','gzip');}
   state.wire_offered=payload.length;res.setHeader('content-type','application/json');res.setHeader('connection','close');
   if(owned.mode==='over_hold'){res.writeHead(200);res.write(payload,()=>{state.flushed=true;});}
   else res.end(payload,()=>{state.flushed=true;});
  });
  origin.on('connection',socket=>{sockets.add(socket);socket.once('close',()=>sockets.delete(socket));});
  origin.on('tlsClientError',error=>{state.error=`${error.name}: ${error.message}`;});
  control=http.createServer((req,res)=>{res.setHeader('content-type','application/json');res.setHeader('connection','close');if(req.method==='POST'&&req.url==='/stop')res.once('finish',()=>{for(const socket of sockets)socket.destroy();origin.close();control.close();});res.end(JSON.stringify(state));});
  await new Promise(resolve=>origin.listen(0,'127.0.0.1',resolve));await new Promise(resolve=>control.listen(0,'127.0.0.1',resolve));
  process.stdout.write(JSON.stringify({origin:origin.address().port,control:control.address().port,root_der:owned.root_der})+'\n');
 }catch(error){for(const socket of sockets)socket.destroy();origin?.close();control?.close();process.stderr.write(`owned price fixture: ${error.name}: ${error.message}\n`);process.exitCode=1;}
});
