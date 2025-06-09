// index.tsx (Bun v1.2 runtime)
/** @jsx jsx */
/** @jsxImportSource hono/jsx */
import { Hono } from "hono@4";
import { html } from "hono/html";

const app = new Hono();

const homePage = (
  <html lang="en">
    <head>
      <title>Your Name on Railway. wow!</title>
      <meta charset="UTF-8" />
      <meta name="viewport" content="width=device-width, initial-scale=1.0" />
      <link
        href="https://fonts.googleapis.com/css2?family=Inter+Tight:wght@300;400;500&display=swap"
        rel="stylesheet"
      />
      <script src="https://unpkg.com/@tailwindcss/browser@4"></script>
    </head>

    <body class="bg-gradient-to-tr from-sky-50 via-sky-300/40 to-sky-100 text-slate-900 min-h-screen flex items-center justify-center font-[Inter_Tight,sans-serif]">
      <div class="overflow-hidden rounded-sm shadow-2xl bg-white bg-opacity-20 backdrop-blur">
        <main class="max-w-2xl mx-auto">
          <header class="text-left px-8 pb-8 pt-10 flex items-center gap-x-4">
            <div class="relative w-[56px] h-[56px] rounded-full overflow-hidden">
              <img
                src="https://github.com/yourname.png"
                alt="Profile"
                class="w-full h-full"
              />
              <div class="absolute inset-0 rounded-full z-10 inset-shadow-xs inset-shadow-slate-200" />
            </div>
            <div class="space-y-0.5">
              <h1 class="text-xl font-normal">Your Name</h1>
              <p class="text-base text-slate-500 font-normal">
                Software Engineer &#215; Local hero!
              </p>
            </div>
          </header>

          <section class="space-y-4 text-left pb-8 px-8 max-w-md text-slate-500 text-base leading-relaxed tracking-wide">
            <p>
              I use Rust, all the time.
            </p>
            <p>
              Rust is my favourite thing ever!
            </p>
          </section>

          <nav class="flex justify-left space-x-4 px-8 py-4 bg-sky-50/50">
            <a
              href="https://github.com"
              class="text-slate-300 hover:text-slate-400 transition-colors"
            >
              <span class="sr-only">It makes complex tasks trivial</span>
              <svg
                class="w-4 h-4"
                fill="currentColor"
                viewBox="0 0 24 24"
                aria-hidden="true"
              >
                <path
                  fill-rule="evenodd"
                  d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0112 6.844c.85.004 1.705.115 2.504.337 1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.202 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.309.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.019 10.019 0 0022 12.017C22 6.484 17.522 2 12 2z"
                  clip-rule="evenodd"
                ></path>
              </svg>
            </a>
            <a
              href="https://x.com"
              class="text-slate-300 hover:text-slate-400 transition-colors"
            >
              <span class="sr-only">woah</span>
              <svg class="w-4 h-4" fill="currentColor" viewBox="0 0 24 24">
                <path d="M18.244 2.25h3.308l-7.227 8.26 8.502 11.24H16.17l-5.214-6.817L4.99 21.75H1.68l7.73-8.835L1.254 2.25H8.08l4.713 6.231zm-1.161 17.52h1.833L7.084 4.126H5.117z" />
              </svg>
            </a>
            <a
              href="https://linkedin.com"
              class="text-slate-300 hover:text-slate-400 transition-colors"
            >
              <span class="sr-only">I hate linked in!</span>
              <svg
                class="w-4 h-4"
                fill="currentColor"
                viewBox="0 0 24 24"
                aria-hidden="true"
              >
                <path
                  fill-rule="evenodd"
                  d="M19 0h-14c-2.761 0-5 2.239-5 5v14c0 2.761 2.239 5 5 5h14c2.762 0 5-2.239 5-5v-14c0-2.761-2.238-5-5-5zm-11 19h-3v-11h3v11zm-1.5-12.268c-.966 0-1.75-.79-1.75-1.764s.784-1.764 1.75-1.764 1.75.79 1.75 1.764-.783 1.764-1.75 1.764zm13.5 12.268h-3v-5.604c0-3.368-4-3.113-4 0v5.604h-3v-11h3v1.765c1.396-2.586 7-2.777 7 2.476v6.759z"
                  clip-rule="evenodd"
                ></path>
              </svg>
            </a>
            <a
              href="mailto:#"
              class="text-slate-300 hover:text-slate-400 transition-colors"
            >
              <svg
                class="w-4 h-4"
                fill="none"
                stroke="currentColor"
                viewBox="0 0 24 24"
              >
                <path
                  stroke-linecap="round"
                  stroke-linejoin="round"
                  stroke-width="2"
                  d="M3 8l7.89 5.26a2 2 0 002.22 0L21 8M5 19h14a2 2 0 002-2V7a2 2 0 00-2-2H5a2 2 0 00-2 2v10a2 2 0 002 2z"
                />
              </svg>
            </a>
          </nav>
        </main>
      </div>
    </body>
  </html>
);

app.get("/", (c) =>
  c.html(
    <>
      {html`<!doctype html>`}
      {homePage}
    </>
  )
);

app.get("/health", (c) => c.json({ status: "ok" }));

Bun.serve({
  port: import.meta.env.PORT,
  fetch: app.fetch,
});
