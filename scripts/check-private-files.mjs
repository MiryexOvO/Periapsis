import { execFileSync } from 'node:child_process';

// 只读取文件名，阻止私有研发目录进入提交或推送，不读取文档内容。
const push = process.argv.includes('--push');
const args = push
  ? ['log', '--format=', '--name-only', '-z', '--no-renames', '--diff-filter=ACMR', 'HEAD', '--not', '--remotes']
  : ['diff', '--cached', '--name-only', '--diff-filter=ACMR', '-z'];
const paths = execFileSync('git', args, { encoding: 'utf8' }).split('\0').map(path => path.replace(/^\n+/, '')).filter(Boolean);
const forbidden = paths.filter(path => /^specs\//i.test(path));
if (forbidden.length) {
  console.error(`Private specs files must not be ${push ? 'pushed' : 'committed'}:\n${forbidden.join('\n')}`);
  process.exitCode = 1;
}
