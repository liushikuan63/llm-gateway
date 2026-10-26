const test=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');const vm=require('node:vm');const ts=require('typescript');
const m={exports:{}};vm.runInNewContext(ts.transpileModule(fs.readFileSync('src/pages/model-labels.ts','utf8'),{compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2020}}).outputText,{module:m,exports:m.exports});
const {modelDisplayLabel}=m.exports;
test('official labels improve display while caller alias and wire ID stay unchanged',()=>{
 const model={alias:'qfmodel',upstream:'qfmodel'};const before=JSON.stringify(model);
 assert.equal(modelDisplayLabel({model_display_names:{qfmodel:'Qwen3.8-Flash'}},model),'Qwen3.8-Flash');assert.equal(JSON.stringify(model),before);
 assert.equal(modelDisplayLabel({model_display_names:{custom:'DeepSeek-Flash'}},{alias:'custom',upstream:'dfmodel'}),'DeepSeek-Flash（custom）');
});
test('missing invalid or control-filled display metadata preserves the existing alias',()=>{
 const model={alias:'unknown-model',upstream:'unknown-model'};
 for(const name of [null,1,'','\nforged','x'.repeat(257)]) assert.equal(modelDisplayLabel({model_display_names:{'unknown-model':name}},model),'unknown-model');
 assert.equal(modelDisplayLabel({},model),'unknown-model');
});

test('saved Trae display metadata stays separate from opaque selector aliases and wire config names',()=>{
 const model={alias:'trae-cn-owned-selector',upstream:'owned-wire-config'};
 const before=JSON.stringify(model);
 assert.equal(modelDisplayLabel({model_display_names:{[model.alias]:'Owned catalog display'}},model),'Owned catalog display（trae-cn-owned-selector）');
 assert.equal(JSON.stringify(model),before);
 assert.equal(modelDisplayLabel({model_display_names:{other:'Unrelated label'}},model),model.alias);
});
