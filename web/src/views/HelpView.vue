<script setup lang="ts">
import { computed } from "vue";
import type { Status } from "../lib/types";

const props = defineProps<{ status: Status | null }>();

const host = computed(() => {
  const ip = (props.status?.local_ips ?? []).find(
    (x) => !x.startsWith("127.") && x !== "::1",
  );
  return ip ?? "192.168.1.10";
});
const port = computed(() => props.status?.record_port ?? 8888);
</script>

<template>
  <div class="max-w-[860px] space-y-8">
    <section>
      <h2 class="text-[15px] font-medium">方式一 · 测试包里的代理开关</h2>
      <p class="mt-1.5 text-[12.5px] leading-6 text-muted">
        零网络配置，只影响 App 自己的请求；需要在盒子上安装带开关的测试包。
      </p>
      <ol class="mt-3 space-y-2 text-[13px]">
        <li class="flex gap-3">
          <span class="readout w-4 shrink-0 text-faint">1</span>
          <span>打开测试包设置页，开启「使用 TAPE 代理」。</span>
        </li>
        <li class="flex gap-3">
          <span class="readout w-4 shrink-0 text-faint">2</span>
          <span>
            IP 填 <code class="readout text-[12.5px]">{{ host }}</code>，端口填
            <code class="readout text-[12.5px]">{{ port }}</code>。
          </span>
        </li>
        <li class="flex gap-3">
          <span class="readout w-4 shrink-0 text-faint">3</span>
          <span>回到「状态」页，看到请求进来即成功；开关旁会显示已连接。</span>
        </li>
      </ol>
    </section>

    <section>
      <h2 class="text-[15px] font-medium">方式二 · 透明模式（盒子上什么都不用装）</h2>
      <p class="mt-1.5 text-[12.5px] leading-6 text-muted">
        让盒子的流量经过本机：盒子的网关指向这台电脑，tape 按 Host 头转发并录制。
        只支持 http，https 会直连出去。
      </p>
      <ol class="mt-3 space-y-2 text-[13px]">
        <li class="flex gap-3">
          <span class="readout w-4 shrink-0 text-faint">1</span>
          <span>在「设置」里启用透明模式，填好目标端口（例如 80, 8080）。</span>
        </li>
        <li class="flex gap-3">
          <span class="readout w-4 shrink-0 text-faint">2</span>
          <span>把盒子的网关改成 <code class="readout text-[12.5px]">{{ host }}</code>，DNS 可保持不变。</span>
        </li>
        <li class="flex gap-3">
          <span class="readout w-4 shrink-0 text-faint">3</span>
          <span>在本机把 80 等目标端口转到 tape（Linux / macOS 示例）：</span>
        </li>
      </ol>
      <pre class="readout mt-3 overflow-x-auto border border-line bg-[#f7f8f7] px-4 py-3 text-[11.5px] leading-6">iptables -t nat -A PREROUTING -p tcp --dport 80 -j REDIRECT --to-port {{ port }}
# 需要本机开启转发并放行转发流量
sysctl -w net.ipv4.ip_forward=1</pre>
      <p class="mt-2 text-[12px] leading-6 text-muted">
        Windows 自带防火墙无法按端口做透明重定向，通常配合一台小路由器完成，或继续用方式一。
      </p>
    </section>

    <section>
      <h2 class="text-[15px] font-medium">方式三 · 一台小路由器</h2>
      <p class="mt-1.5 text-[12.5px] leading-6 text-muted">
        把盒子接在预配置的小路由器后面，由路由器把 http 流量转到本机 tape。
        盒子的网络设置完全不用改，适合设置被锁死或多人轮用的场景。
      </p>
    </section>

    <section class="border border-line bg-panel">
      <div class="border-b border-line px-6 py-3.5">
        <h2 class="text-[13.5px] font-medium">常见问题</h2>
      </div>
      <dl class="divide-y divide-line text-[13px]">
        <div class="px-6 py-4">
          <dt class="font-medium">盒子上请求失败，「录制」页也没有记录</dt>
          <dd class="mt-1 text-[12.5px] leading-6 text-muted">
            先确认盒子和本机在同一网段，再检查 Windows 防火墙是否放行了
            {{ port }} 端口。地址填错时 App 会探测失败并自动关掉开关，不会卡死。
          </dd>
        </div>
        <div class="px-6 py-4">
          <dt class="font-medium">透明模式开启了，但请求没进来</dt>
          <dd class="mt-1 text-[12.5px] leading-6 text-muted">
            看目标端口是否在白名单里；盒子的接口如果是 https，或者直连 IP 且端口不在名单内，都会被放过。
          </dd>
        </div>
        <div class="px-6 py-4">
          <dt class="font-medium">回放时页面能打开，接口 404</dt>
          <dd class="mt-1 text-[12.5px] leading-6 text-muted">
            说明这条请求没有录到。回到录制阶段，确认该接口的 host / 端口在白名单内，并且没有被「录制范围」过滤掉。
          </dd>
        </div>
        <div class="px-6 py-4">
          <dt class="font-medium">数据存在哪里</dt>
          <dd class="mt-1 text-[12.5px] leading-6 text-muted">
            <code class="readout text-[12px]">{{ status?.dir ?? "./tape-api" }}</code>
            下的 <code class="readout text-[12px]">snapshots/</code>，按站点分目录，一条请求一个 JSON。
          </dd>
        </div>
      </dl>
    </section>
  </div>
</template>