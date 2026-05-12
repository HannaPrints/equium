import Link from "next/link";
import { Navbar } from "@/components/Navbar";
import { Footer } from "@/components/Footer";

export const metadata = {
  title: "Mine Equium with your GPU · install guide",
  description:
    "Mine $EQM with the open-source GPU miner. Cross-platform via wgpu — Metal, Vulkan, DX12. Build from source in 5 minutes on macOS, Linux, or Windows. CPU fallback included.",
};

export default function DownloadPage() {
  return (
    <main>
      <Navbar />
      <div className="pt-32 pb-20 px-6">
        <div className="max-w-3xl mx-auto">
          <div className="text-[11px] font-mono uppercase tracking-[0.2em] text-[var(--color-rose)] mb-3 font-semibold">
            Mine Equium
          </div>
          <h1 className="text-[40px] md:text-[52px] font-black tracking-[-0.025em] leading-[1.05] mb-5">
            Mine $EQM with your GPU.
          </h1>
          <p className="text-[17px] leading-[1.6] text-[var(--color-fg-dim)] max-w-2xl mb-3">
            Single Rust binary. No CUDA, no Electron, no proprietary
            driver — cross-platform via <Code>wgpu</Code>. Any modern
            GPU works; CPU and{" "}
            <Link
              href="/mine"
              className="text-[var(--color-rose)] hover:underline"
            >
              browser
            </Link>{" "}
            fallbacks ship too.
          </p>
          <p className="text-[14px] leading-[1.6] text-[var(--color-fg-faint)] max-w-2xl mb-10">
            Renting a GPU is the easiest path — start there if you
            don't already have one.
          </p>

          {/* OS picker — each section installs the GPU miner */}
          <div className="grid grid-cols-2 sm:grid-cols-4 gap-3 mb-10">
            <OsCard
              label="Rent a GPU"
              hint="vast.ai / Runpod · easiest"
              href="#cloud"
            />
            <OsCard
              label="macOS"
              hint="Metal · Apple Silicon or Intel"
              href="#macos"
            />
            <OsCard
              label="Linux"
              hint="Vulkan · NVIDIA, AMD, Intel"
              href="#linux"
            />
            <OsCard
              label="Windows"
              hint="DX12 or Vulkan via WSL2"
              href="#windows"
            />
          </div>

          {/* Cloud GPU rental — the easy path */}
          <Section id="cloud" title="Rent a GPU and mine in 3 steps.">
            <Callout>
              <strong>No GPU at home? Rent one for ~$0.20/hr.</strong>{" "}
              <a
                href="https://cloud.vast.ai?ref_id=536464&template_id=374ef07348ceb9164823cc755e06fd9c"
                target="_blank"
                rel="noreferrer noopener"
                className="text-[var(--color-rose)] hover:underline"
              >
                vast.ai
              </a>{" "}
              and{" "}
              <a
                href="https://runpod.io"
                target="_blank"
                rel="noreferrer noopener"
                className="text-[var(--color-rose)] hover:underline"
              >
                Runpod
              </a>{" "}
              both rent NVIDIA cards by the hour with one-click Ubuntu
              + CUDA images. We don't use CUDA directly — the miner
              talks to the GPU via Vulkan, which NVIDIA's driver
              supports natively — so any of the standard "PyTorch /
              CUDA 12" templates work out of the box.
            </Callout>

            <Block label="1 · Pick an instance">
              <P>
                We haven't benchmarked specific cards yet — these are
                starting recommendations based on Equihash 96,5 being
                memory-bandwidth bound. Mid-range NVIDIAs are usually
                better $/hashrate than flagships.
              </P>

              {/* CTA: opens the marketplace pre-filtered to instances
                  compatible with our official template (Ubuntu 22.04 +
                  CUDA 12.4 + verified host + sane sizing). Clicking
                  through pre-populates the rental dialog so the user
                  just hits "Rent" → SSH → ./cloud-mine.sh. */}
              <div className="my-5">
                <a
                  href="https://cloud.vast.ai?ref_id=536464&template_id=374ef07348ceb9164823cc755e06fd9c"
                  target="_blank"
                  rel="noreferrer noopener"
                  className="inline-flex items-center gap-2 px-5 py-3 rounded-2xl bg-[var(--color-rose)] text-[var(--color-bg)] font-bold text-[14px] hover:bg-[var(--color-rose-bright)] transition-colors"
                >
                  Rent a GPU with our template
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
                    <path d="M7 17 17 7" />
                    <path d="M7 7h10v10" />
                  </svg>
                </a>
                <span className="ml-3 text-[12px] font-mono text-[var(--color-fg-dim)]">
                  Ubuntu 22.04 · CUDA 12.4 · pre-filtered
                </span>
              </div>

              <ul className="list-disc pl-6 space-y-1.5 text-[14px] leading-[1.6] text-[var(--color-fg-dim)] my-4">
                <li>
                  <strong>Start mid-range.</strong> RTX 30/40-series
                  6-12 GB cards (3060, 4060, 3070, 4070) often beat
                  flagships on $/hashrate — flagship ALUs don't help a
                  memory-bound workload. Report your numbers and we'll
                  publish a real comparison.
                </li>
                <li>
                  <strong>One GPU is plenty.</strong> The miner uses a
                  single device.
                </li>
                <li>
                  <strong>On-demand, not interruptible.</strong> Spot
                  instances get killed mid-block.
                </li>
                <li>
                  <strong>Verified host + Ubuntu 22.04 PyTorch/CUDA
                  template.</strong> Saves driver-debugging time.
                </li>
                <li>
                  <strong>Check the Driver Version column.</strong>{" "}
                  535 / 545 / 550 are confirmed working;{" "}
                  <strong>avoid 555 / 565 / 570 / 575</strong> — that
                  open-driver branch crashes our compute pipelines
                  with a SPIR-V compiler bug. Most current vast.ai
                  hosts run 555+, so our template can't filter them
                  out structurally — eyeball the column on each
                  candidate. If you land on a bad one anyway, the
                  miner's auto-probe tells you within ~5 seconds and
                  you can stop the rental before it costs anything.
                </li>
              </ul>
            </Block>

            <Block label="2 · SSH in and run the bootstrap">
              <Pre>{`./cloud-mine.sh`}</Pre>
              <P>
                The template's on-start script stages{" "}
                <Code>cloud-mine.sh</Code> at <Code>~/cloud-mine.sh</Code>{" "}
                and prints a reminder in the SSH MOTD. Running it
                installs Rust + Vulkan + Solana CLI if they're missing,
                clones the repo, builds <Code>equium-gpu-miner</Code>,
                generates a mining keypair, and runs <Code>verify</Code>{" "}
                so you can see your rented GPU is detected. Then it
                prints a Solana address and waits.
              </P>
              <Callout tone="dim">
                Not using our template? Same command works on any
                Linux box —{" "}
                <Code>
                  curl -fsSL https://raw.githubusercontent.com/HannaPrints/equium/master/scripts/cloud-mine.sh
                  -o cloud-mine.sh &amp;&amp; chmod +x cloud-mine.sh
                </Code>
                .
              </Callout>
            </Block>

            <Block label="3 · Fund + mine">
              <P>
                Send ~0.01 SOL to the printed address for tx fees, and
                paste a Helius RPC URL when prompted. The script polls
                the balance until the funds land, then execs the miner.
                That's it.
              </P>
              <Callout tone="dim">
                <strong>The script handles every weird case for you.</strong>{" "}
                The miner's backend auto-probe runs each candidate in
                a subprocess (so a driver SIGSEGV in{" "}
                <Code>libnvidia-glvkspirv.so</Code> kills only the
                probe child, not your shell), and{" "}
                <Code>cloud-mine.sh</Code> detects NVIDIA 575.x via{" "}
                <Code>nvidia-smi</Code> and offers a one-keypress{" "}
                <Code>apt install nvidia-driver-535-server</Code> + reboot
                — the permanent fix for that branch's SPIR-V crash. After
                reboot, rerun the script and it skips straight to mining
                (RPC URL is saved at <Code>~/.config/equium/rpc</Code>).
                Override the backend choice via{" "}
                <Code>EQUIUM_BACKEND=vulkan|gl|metal|dx12</Code> if you
                ever need to.
              </Callout>
              <Callout tone="dim">
                <strong>Something not working?</strong> Run{" "}
                <Code>./cloud-mine.sh --doctor</Code> for a one-page
                redacted report (OS / GPU / drivers / Vulkan / tool
                versions / repo state / recent logs) you can paste
                into a GitHub issue. Helius API keys + tokens are
                stripped automatically; keypair contents are never
                read.
              </Callout>
              <Callout tone="dim">
                <strong>Keep the keypair if you want to keep mining.</strong>{" "}
                vast.ai instances are ephemeral — when you stop the
                rental, the box goes away. Copy{" "}
                <Code>~/.config/solana/id.json</Code> off the box
                before destroying the instance, or your mined EQM stays
                tied to a key only that instance held. The{" "}
                <Link href="/mine" className="text-[var(--color-rose)] hover:underline">
                  browser miner
                </Link>{" "}
                gives you the same wallet on any device for free, FYI.
              </Callout>
            </Block>
          </Section>

          {/* Common prerequisites */}
          <section className="mb-12">
            <h2 className="text-[22px] font-bold tracking-[-0.015em] mb-3">
              What you'll need
            </h2>
            <ul className="list-disc pl-6 space-y-2 text-[14.5px] leading-[1.65] text-[var(--color-fg-dim)]">
              <li>
                A Solana keypair file. Generate one with{" "}
                <Code>solana-keygen new -o ~/.config/solana/id.json</Code>{" "}
                (the Solana CLI install instructions are part of each section
                below).
              </li>
              <li>
                A small amount of SOL for transaction fees in that keypair's
                address. Roughly 0.01 SOL covers a few hours of mining.
              </li>
              <li>
                A Solana RPC endpoint. A free Helius key is fine —{" "}
                <Link
                  href="/docs/rpc"
                  className="text-[var(--color-rose)] hover:underline"
                >
                  5-minute setup
                </Link>
                . The default public mainnet endpoint will rate-limit you out
                of meaningful mining within seconds.
              </li>
            </ul>
          </section>

          {/* macOS */}
          <Section id="macos" title="macOS">
            <P>
              Native macOS — Apple Silicon or Intel, both work. Open Terminal:
            </P>
            <Block label="1 · Install Rust">
              <Pre>{`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"`}</Pre>
            </Block>
            <Block label="2 · Install the Solana CLI (for keypair generation)">
              <Pre>{`sh -c "$(curl -sSfL https://release.anza.xyz/stable/install)"
export PATH="$HOME/.local/share/solana/install/active_release/bin:$PATH"`}</Pre>
            </Block>
            <Block label="3 · Generate a mining keypair">
              <Pre>{`solana-keygen new -o ~/.config/solana/id.json --no-bip39-passphrase
solana-keygen pubkey ~/.config/solana/id.json   # send a bit of SOL here`}</Pre>
            </Block>
            <Block label="4 · Build + run the GPU miner">
              <Pre>{`git clone https://github.com/HannaPrints/equium.git
cd equium
cargo build --release -p equium-gpu-miner

# Quick sanity check — auto-probes Metal and confirms the shader
# matches the CPU reference byte-for-byte.
./target/release/equium-gpu-miner verify

# --full-gpu runs all five Wagner rounds on the GPU. Drops the flag
# if you want the hybrid path (CPU does Wagner, GPU does BLAKE2b).
./target/release/equium-gpu-miner mine --full-gpu \\
  --rpc-url https://mainnet.helius-rpc.com/?api-key=YOUR_KEY \\
  --keypair ~/.config/solana/id.json`}</Pre>
              <P>
                Apple Silicon talks to the GPU over Metal; everything
                stays on-device, no driver install. Once it's mining,
                see{" "}
                <a href="#advanced" className="text-[var(--color-rose)] hover:underline">
                  Tune your GPU miner
                </a>{" "}
                for benchmarks.
              </P>
            </Block>
          </Section>

          {/* Linux */}
          <Section id="linux" title="Linux">
            <Callout>
              <strong>Easiest: one command does everything.</strong>{" "}
              The same installer that powers vast.ai mining works on
              any Linux box. It installs Rust, the Solana CLI, Vulkan,
              and{" "}
              <Code>nvcc</Code> (on NVIDIA), builds the miner with the{" "}
              CUDA backend, and starts mining with{" "}
              <Code>--full-gpu</Code>.
              <Pre>{`curl -fsSL https://raw.githubusercontent.com/HannaPrints/equium/master/scripts/cloud-mine.sh \\
  -o ~/cloud-mine.sh && chmod +x ~/cloud-mine.sh && ~/cloud-mine.sh`}</Pre>
              <span className="text-[12px] text-[var(--color-fg-dim)]">
                Re-runnable. Saves the keypair + RPC URL so Ctrl-C +
                rerun resumes mining instantly. Prefer to build by
                hand? Manual steps below.
              </span>
            </Callout>
            <P>
              Tested on Ubuntu 22.04, Debian 12, Arch, and Fedora. Other
              distros work the same — adjust the package manager call.
            </P>
            <Block label="1 · Install build tools">
              <Pre>{`# Debian / Ubuntu
sudo apt update && sudo apt install -y build-essential pkg-config libssl-dev curl git

# Arch
sudo pacman -S --needed base-devel openssl curl git

# Fedora
sudo dnf install -y @development-tools openssl-devel curl git`}</Pre>
            </Block>
            <Block label="2 · Install Rust">
              <Pre>{`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"`}</Pre>
            </Block>
            <Block label="3 · Install the Solana CLI">
              <Pre>{`sh -c "$(curl -sSfL https://release.anza.xyz/stable/install)"
export PATH="$HOME/.local/share/solana/install/active_release/bin:$PATH"`}</Pre>
            </Block>
            <Block label="4 · Generate a keypair">
              <Pre>{`solana-keygen new -o ~/.config/solana/id.json --no-bip39-passphrase
solana-keygen pubkey ~/.config/solana/id.json`}</Pre>
            </Block>
            <Block label="5 · Build + run the GPU miner">
              <Pre>{`# Vulkan loader (the wgpu fallback path; AMD/Intel use it,
# NVIDIA on healthy drivers does too).
# Debian / Ubuntu
sudo apt install -y libvulkan1 vulkan-tools
# Arch
sudo pacman -S vulkan-icd-loader vulkan-tools
# Fedora
sudo dnf install -y vulkan-loader vulkan-tools

# NVIDIA only — install nvcc so we can build the CUDA backend.
# Skip this on AMD/Intel.
sudo apt install -y nvidia-cuda-toolkit

git clone https://github.com/HannaPrints/equium.git
cd equium

# NVIDIA → enable the CUDA backend (fastest path, also bypasses
# the SPIR-V crash on driver 555-575).
cargo build --release -p equium-gpu-miner --features cuda
# AMD / Intel / no nvcc → drop --features cuda:
# cargo build --release -p equium-gpu-miner

# Sanity check. The miner self-probes CUDA → Vulkan → GL in
# subprocesses so a buggy driver kills the probe, not your terminal.
./target/release/equium-gpu-miner verify

# --full-gpu runs all five Wagner rounds on the GPU. On a 4090 with
# CUDA this is ~20× the hybrid path. Drop the flag for hybrid mode.
./target/release/equium-gpu-miner mine --full-gpu \\
  --rpc-url https://mainnet.helius-rpc.com/?api-key=YOUR_KEY \\
  --keypair ~/.config/solana/id.json`}</Pre>
              <P>
                Works on NVIDIA (CUDA or Vulkan), AMD, and Intel GPUs
                that support Vulkan 1.1+ (or GL 4.5 as fallback). If{" "}
                <Code>vulkaninfo</Code> lists your adapter or{" "}
                <Code>nvidia-smi</Code> shows a healthy GPU, the miner
                will pick it up. Once mining works, see{" "}
                <a href="#advanced" className="text-[var(--color-rose)] hover:underline">
                  Tune your GPU miner
                </a>
                .
              </P>
            </Block>
          </Section>

          {/* Windows */}
          <Section id="windows" title="Windows">
            <Callout>
              <strong>WSL2 is the path of least resistance.</strong> Native
              Windows + Rust + Solana CLI is theoretically possible but
              accumulates papercuts fast (path handling, OpenSSL, line
              endings). Run Linux inside Windows via WSL2 and skip them.
            </Callout>
            <Callout>
              <strong>Easiest: one command inside WSL.</strong> After
              installing WSL2 + Ubuntu (step 1), drop into Ubuntu and
              run:
              <Pre>{`curl -fsSL https://raw.githubusercontent.com/HannaPrints/equium/master/scripts/cloud-mine.sh \\
  -o ~/cloud-mine.sh && chmod +x ~/cloud-mine.sh && ~/cloud-mine.sh`}</Pre>
              <span className="text-[12px] text-[var(--color-fg-dim)]">
                Installs everything (Rust, Solana CLI, Vulkan,{" "}
                <Code>nvcc</Code> for NVIDIA), builds the miner, and
                starts mining with <Code>--full-gpu</Code>. Skip steps
                2-5 below.
              </span>
            </Callout>

            <Block label="1 · Install WSL2 + Ubuntu (one-time)">
              <P>
                Open <strong>PowerShell as Administrator</strong> and run:
              </P>
              <Pre>{`wsl --install -d Ubuntu`}</Pre>
              <P>
                Reboot when prompted, then open the new "Ubuntu" app from
                the Start menu and set a UNIX username + password. You're
                now inside Linux — everything below runs in the Ubuntu
                terminal, not PowerShell.
              </P>
            </Block>

            <Block label="2 · Install build tools (inside WSL)">
              <Pre>{`sudo apt update && sudo apt install -y build-essential pkg-config libssl-dev curl git`}</Pre>
            </Block>

            <Block label="3 · Install Rust">
              <Pre>{`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"`}</Pre>
            </Block>

            <Block label="4 · Install the Solana CLI">
              <Pre>{`sh -c "$(curl -sSfL https://release.anza.xyz/stable/install)"
export PATH="$HOME/.local/share/solana/install/active_release/bin:$PATH"`}</Pre>
            </Block>

            <Block label="5 · Generate a keypair + build the GPU miner">
              <Pre>{`solana-keygen new -o ~/.config/solana/id.json --no-bip39-passphrase
solana-keygen pubkey ~/.config/solana/id.json    # send SOL here

# WSL2 exposes your Windows GPU via Vulkan (NVIDIA + AMD have full
# support; Intel works on recent drivers). Install Vulkan + nvcc
# (the latter only if you have an NVIDIA card):
sudo apt install -y libvulkan1 vulkan-tools
sudo apt install -y nvidia-cuda-toolkit   # NVIDIA only

git clone https://github.com/HannaPrints/equium.git
cd equium

# NVIDIA → enable the CUDA backend. AMD/Intel → drop --features cuda.
cargo build --release -p equium-gpu-miner --features cuda

# Sanity check. Auto-probes CUDA → Vulkan → GL in subprocesses so
# a buggy driver kills the probe, not your shell.
./target/release/equium-gpu-miner verify

# --full-gpu = all five Wagner rounds on the GPU. The fast path.
./target/release/equium-gpu-miner mine --full-gpu \\
  --rpc-url https://mainnet.helius-rpc.com/?api-key=YOUR_KEY \\
  --keypair ~/.config/solana/id.json`}</Pre>
            </Block>

            <Callout tone="dim">
              <strong>Native Windows build?</strong> Run the same{" "}
              <Code>cargo build</Code> from PowerShell after installing
              rustup-init.exe, Build Tools for Visual Studio (C++
              workload), and the Solana Windows installer. wgpu picks
              up DX12 automatically. File a GitHub issue if something
              specific breaks and we'll document a workaround.
            </Callout>
          </Section>

          {/* Tune your GPU miner */}
          <Section id="advanced" title="Tune & verify">
            <Block label="Sanity-check your driver">
              <Pre>{`./target/release/equium-gpu-miner verify`}</Pre>
              <P>
                Auto-probes Vulkan → GL in subprocesses (so a driver
                SIGSEGV kills only the probe child), then compares
                the WGSL leaves output against the CPU reference. A
                healthy box prints{" "}
                <Code>✓ GPU output matches CPU reference byte-for-byte</Code>.
              </P>
            </Block>

            <Block label="Full-GPU mode (--full-gpu)">
              <Pre>{`./target/release/equium-gpu-miner verify-rounds --nonces 4
./target/release/equium-gpu-miner mine --full-gpu \\
  --rpc-url https://mainnet.helius-rpc.com/?api-key=YOUR_KEY \\
  --keypair ~/.config/solana/id.json`}</Pre>
              <P>
                Runs all five Wagner rounds + the solution scan on the
                GPU instead of the CPU. On NVIDIA with the CUDA backend
                this is ~20× the hybrid path (e.g. ~320 H/s on a 4090
                vs ~15 H/s hybrid).{" "}
                <Code>verify-rounds</Code> validates the GPU pipeline
                against the CPU reference at every round, on your
                specific driver, before you commit.
              </P>
            </Block>

            <Callout tone="dim">
              <Code>bench</Code> prints BLAKE2b throughput at full
              width if you want a number. Full source + roadmap at{" "}
              <a
                href="https://github.com/HannaPrints/equium/tree/master/clients/gpu-miner"
                className="text-[var(--color-rose)] hover:underline"
                target="_blank"
                rel="noreferrer noopener"
              >
                clients/gpu-miner
              </a>
              .
            </Callout>
          </Section>

          {/* CPU fallback */}
          <Section id="cpu" title="No GPU? CPU still earns.">
            <P>
              Equihash 96,5 is memory-bound, so commodity CPUs stay
              viable — auto-retargeting tracks total network hashrate,
              and your share scales with yours.
            </P>
            <Block label="Build + run the CPU miner">
              <Pre>{`cd equium
cargo build --release -p equium-cli-miner
./target/release/equium-miner \\
  --rpc-url https://mainnet.helius-rpc.com/?api-key=YOUR_KEY \\
  --keypair ~/.config/solana/id.json \\
  --threads $(nproc 2>/dev/null || sysctl -n hw.physicalcpu)`}</Pre>
            </Block>
          </Section>

          <div className="rounded-2xl border border-[var(--color-border)] bg-[var(--color-bg-elev)] p-5 md:p-6 mt-12">
            <h3 className="text-[16px] font-bold tracking-[-0.01em] mb-2">
              Stuck?
            </h3>
            <p className="text-[14px] leading-[1.6] text-[var(--color-fg-dim)]">
              The full source is at{" "}
              <a
                href="https://github.com/HannaPrints/equium"
                target="_blank"
                rel="noreferrer noopener"
                className="text-[var(--color-rose)] hover:underline"
              >
                github.com/HannaPrints/equium
              </a>
              . Open an issue if you hit something specific or post in
              the X replies on{" "}
              <a
                href="https://x.com/EquiumEQM"
                target="_blank"
                rel="noreferrer noopener"
                className="text-[var(--color-rose)] hover:underline"
              >
                @EquiumEQM
              </a>
              .
            </p>
          </div>
        </div>
      </div>
      <Footer />
    </main>
  );
}

function OsCard({
  label,
  hint,
  href,
}: {
  label: string;
  hint: string;
  href: string;
}) {
  return (
    <Link
      href={href}
      className="rounded-2xl border border-[var(--color-border)] bg-[var(--color-bg-elev)] p-5 hover:border-[var(--color-rose-soft)] transition-colors group"
    >
      <div className="text-[18px] font-bold tracking-[-0.01em] group-hover:text-[var(--color-rose)] transition-colors">
        {label}
      </div>
      <div className="text-[12px] font-mono text-[var(--color-fg-dim)] mt-1">
        {hint}
      </div>
    </Link>
  );
}

function Section({
  id,
  title,
  children,
}: {
  id: string;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <section id={id} className="scroll-mt-32 mb-14">
      <h2 className="text-[26px] font-bold tracking-[-0.018em] mb-4">
        {title}
      </h2>
      <div className="space-y-4">{children}</div>
    </section>
  );
}

function Block({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div>
      <div className="text-[10px] font-mono uppercase tracking-[0.18em] text-[var(--color-fg-dim)] mb-2 font-semibold">
        {label}
      </div>
      {children}
    </div>
  );
}

function P({ children }: { children: React.ReactNode }) {
  return (
    <p className="text-[14.5px] leading-[1.65] text-[var(--color-fg-soft)]">
      {children}
    </p>
  );
}

function Pre({ children }: { children: React.ReactNode }) {
  return (
    <pre className="rounded-xl border border-[var(--color-border)] bg-[var(--color-bg)] p-4 overflow-x-auto font-mono text-[12.5px] leading-[1.7] text-[var(--color-fg-soft)]">
      {children}
    </pre>
  );
}

function Code({ children }: { children: React.ReactNode }) {
  return (
    <code className="font-mono text-[12.5px] px-1.5 py-0.5 rounded bg-[var(--color-bg)] border border-[var(--color-border)] text-[var(--color-teal)]">
      {children}
    </code>
  );
}

function Callout({
  children,
  tone = "default",
}: {
  children: React.ReactNode;
  tone?: "default" | "dim";
}) {
  const cls =
    tone === "dim"
      ? "border-[var(--color-border)] bg-[var(--color-bg-elev)] text-[var(--color-fg-dim)]"
      : "border-[var(--color-gold)]/40 bg-[var(--color-gold)]/[0.06]";
  return (
    <div
      className={`rounded-2xl border p-5 my-2 text-[14px] leading-[1.6] ${cls}`}
    >
      {children}
    </div>
  );
}
