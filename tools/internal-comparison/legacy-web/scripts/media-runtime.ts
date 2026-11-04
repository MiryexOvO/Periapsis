import { registerHooks } from "node:module";

// 上游 MSF 入口缺少 ESM 文件扩展名；只重定向该包入口，不改依赖目录。
registerHooks({ resolve(specifier, context, nextResolve) {
  const result = nextResolve(specifier, context);
  return specifier === "@moq/msf" ? { ...result, url: new URL("./catalog.js", result.url).href } : result;
} });
export const { startVideoDemo } = await import("../src/video-demo.ts");
