import fs from 'node:fs/promises';
import path from 'node:path';
import {randomUUID} from 'node:crypto';
import {defaultSettings,validateSettings} from '../../src/settings-config.js';
export function createSettingsStore(file){let current=defaultSettings();return {get value(){return structuredClone(current);},async load(){try{current=validateSettings(JSON.parse(await fs.readFile(file,'utf8')));}catch(e){if(e.code!=='ENOENT')throw Error('无法读取设置文件，请检查配置格式');}return this.value;},async save(input){const next=validateSettings(input);await fs.mkdir(path.dirname(file),{recursive:true});const temp=file+'.'+randomUUID()+'.tmp';try{await fs.writeFile(temp,JSON.stringify(next,null,2)+'\n',{mode:0o600});await fs.rename(temp,file);}finally{await fs.rm(temp,{force:true});}current=next;return this.value;}};}
