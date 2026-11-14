import {
  isPlainTextValue,
  localeCandidates,
  localizedAppSummary,
  localizedAppTagline,
  localizedAppTitle,
  localizedSectionLabel,
  parseAppDisplayI18n,
  pickLocaleEntry,
  validLocaleTag,
} from "../src/features/workspace-apps/appLocale";
import { parseNavigation } from "../src/features/workspace-apps/screen/screenLifecycle";
import {
  APP_CHAT_CONTRACT,
  AppChatContractError,
  localizeAppChat,
  parseAppChat,
} from "../src/features/app-shell/chat/contract";

export {};
/** CAD-2314: locale fallback isolation, descriptor i18n validation, and chat localization. */
declare function require(name: string): any;

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

function equal(actual: unknown, expected: unknown, why: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${why}: expected ${e}, got ${a}`);
}

function refusedChat(value: unknown, why: string): void {
  try {
    parseAppChat(value);
  } catch (err) {
    assert(err instanceof AppChatContractError, `${why}: wrong error ${String(err)}`);
    return;
  }
  throw new Error(`${why}: expected AppChatContractError refusal`);
}

function main() {
  // 1. localeCandidates & Traditional Chinese isolation (never zh / zh-CN / zh-Hans).
  equal(localeCandidates("zh-TW"), ["zh-TW", "zh-HK", "zh-Hant"], "zh-TW candidate order");
  equal(localeCandidates("zh-HK"), ["zh-HK", "zh-TW", "zh-Hant"], "zh-HK candidate order");
  equal(
    localeCandidates("zh-Hant-HK"),
    ["zh-Hant-HK", "zh-TW", "zh-HK", "zh-Hant"],
    "zh-Hant-HK candidate order",
  );
  for (const tag of ["zh-TW", "zh-HK", "zh-Hant", "zh-Hant-HK"]) {
    const candidates = localeCandidates(tag).map((c) => c.toLowerCase());
    assert(!candidates.includes("zh"), `${tag} never falls back to bare zh`);
    assert(!candidates.includes("zh-cn"), `${tag} never falls back to zh-CN`);
    assert(!candidates.includes("zh-hans"), `${tag} never falls back to zh-Hans`);
  }
  equal(localeCandidates("fr-CA"), ["fr-CA", "fr"], "fr-CA falls back to base fr");

  equal(
    pickLocaleEntry({ "zh-CN": { title: "客户关系管理" } }, "zh-TW"),
    undefined,
    "Simplified Chinese entry is never selected for zh-TW",
  );
  equal(
    pickLocaleEntry({ "zh-HK": { title: "客戶關係管理" } }, "zh-TW"),
    { title: "客戶關係管理" },
    "zh-TW accepts zh-HK fallback",
  );

  // 2. Tag/text helpers, parseAppDisplayI18n bounds, and display resolution precedence.
  assert(validLocaleTag("zh-TW"), "validLocaleTag accepts zh-TW");
  assert(!validLocaleTag("zh_TW"), "validLocaleTag rejects underscore");
  assert(isPlainTextValue("Hello", 10), "isPlainTextValue accepts trimmed text");
  assert(!isPlainTextValue("bad\nline", 20), "isPlainTextValue rejects control characters");

  const parsedI18n = parseAppDisplayI18n({
    "zh-TW": { title: " 客戶管理 ", summary: "管理客戶資料", tagline: "客戶關係與分群" },
  });
  equal(
    parsedI18n,
    { "zh-TW": { title: "客戶管理", summary: "管理客戶資料", tagline: "客戶關係與分群" } },
    "parseAppDisplayI18n trims valid entries",
  );
  equal(parseAppDisplayI18n({}), null, "parseAppDisplayI18n rejects empty map");
  equal(parseAppDisplayI18n({ zh_TW: { title: "壞" } }), null, "parseAppDisplayI18n rejects bad tag");
  equal(
    parseAppDisplayI18n({
      en: { title: "1" },
      fr: { title: "2" },
      de: { title: "3" },
      es: { title: "4" },
      ja: { title: "5" },
      ko: { title: "6" },
      pt: { title: "7" },
      it: { title: "8" },
      nl: { title: "9" },
    }),
    null,
    "parseAppDisplayI18n rejects >8 locales",
  );
  equal(
    parseAppDisplayI18n({ "zh-TW": { title: "bad\u0007bell" } }),
    null,
    "parseAppDisplayI18n rejects control characters",
  );
  equal(
    parseAppDisplayI18n({ "zh-TW": { title: "x".repeat(81) } }),
    null,
    "parseAppDisplayI18n rejects title > 80 chars",
  );
  equal(
    parseAppDisplayI18n({ "zh-TW": { summary: "x".repeat(161) } }),
    null,
    "parseAppDisplayI18n rejects summary > 160 chars",
  );
  equal(
    parseAppDisplayI18n({ "zh-TW": { tagline: "x".repeat(81) } }),
    null,
    "parseAppDisplayI18n rejects tagline > 80 chars",
  );

  const dictT = (key: string) =>
    ({
      "CRM": "DICT-CRM",
      "Contacts summary": "DICT-SUMMARY",
      "Contacts tagline": "DICT-TAGLINE",
      "Contacts": "DICT-SECTION",
    })[key] ?? key;

  const precedenceSource = {
    title: "CRM",
    summary: "Contacts summary",
    tagline: "Contacts tagline",
    i18n: { "zh-TW": { title: "TOP-CRM", summary: "TOP-SUMMARY", tagline: "TOP-TAGLINE" } },
    listing: {
      tagline: "Listing tagline",
      i18n: {
        "zh-TW": { title: "LIST-CRM", summary: "LIST-SUMMARY", tagline: "LIST-TAGLINE" },
      },
    },
  };
  equal(localizedAppTitle(precedenceSource, "zh-TW", dictT), "TOP-CRM", "app.i18n wins over listing.i18n and dict");
  equal(localizedAppSummary(precedenceSource, "zh-TW", dictT), "TOP-SUMMARY", "summary top i18n wins");
  equal(localizedAppTagline(precedenceSource, "zh-TW", dictT), "TOP-TAGLINE", "tagline top i18n wins");
  equal(
    localizedAppTitle({ ...precedenceSource, i18n: undefined }, "zh-TW", dictT),
    "LIST-CRM",
    "listing.i18n wins over dictionary fallback",
  );
  equal(
    localizedAppTitle({ title: "CRM", i18n: { "zh-CN": { title: "SC-CRM" } } }, "zh-TW", dictT),
    "DICT-CRM",
    "dict fallback runs when zh-TW not present and ignores zh-CN",
  );
  equal(
    localizedAppTitle({ title: "CRM", i18n: { "zh-TW": { title: "TOP-CRM" } } }, "en-US", dictT),
    "CRM",
    "English locale bypasses dictionary fallback when en not specified",
  );
  equal(
    localizedSectionLabel({ label: "Contacts", i18n: { "zh-HK": { label: "聯絡人-HK" } } }, "zh-TW", dictT),
    "聯絡人-HK",
    "section i18n overrides dictionary with zh-HK->zh-TW fallback",
  );
  equal(
    localizedSectionLabel({ label: "Contacts" }, "zh-TW", dictT),
    "DICT-SECTION",
    "section falls back to dictionary for non-English",
  );
  equal(
    localizedSectionLabel({ label: "Contacts" }, "en", dictT),
    "Contacts",
    "section stays English for en",
  );

  // 3. parseNavigation with sections[].i18n bounds.
  const validNav = {
    section_query: "section",
    default_section: "contacts",
    sections: [
      {
        id: "contacts",
        label: "Contacts",
        i18n: { "zh-TW": { label: "聯絡人" }, "zh-HK": { label: "聯絡人" } },
      },
    ],
  };
  const parsedNav = parseNavigation(validNav);
  assert(parsedNav !== null, "valid sections[].i18n parses");
  equal(parsedNav.sections[0].i18n?.["zh-TW"]?.label, "聯絡人", "parsed navigation preserves section i18n");

  const navWithI18n = (i18n: unknown) => ({
    section_query: "section",
    default_section: "contacts",
    sections: [{ id: "contacts", label: "Contacts", i18n }],
  });
  equal(parseNavigation(navWithI18n({ zh_TW: { label: "聯絡人" } })), null, "parseNavigation rejects bad locale tag");
  equal(parseNavigation(navWithI18n({})), null, "parseNavigation rejects empty i18n map");
  equal(
    parseNavigation(
      navWithI18n({
        en: { label: "a" },
        fr: { label: "b" },
        de: { label: "c" },
        es: { label: "d" },
        ja: { label: "e" },
        ko: { label: "f" },
        pt: { label: "g" },
        it: { label: "h" },
        nl: { label: "i" },
      }),
    ),
    null,
    "parseNavigation rejects >8 locales",
  );
  equal(parseNavigation(navWithI18n({ "zh-TW": { label: "壞\n字" } })), null, "parseNavigation rejects control chars");
  equal(parseNavigation(navWithI18n({ "zh-TW": { label: "   " } })), null, "parseNavigation rejects whitespace label");
  equal(parseNavigation(navWithI18n({ "zh-TW": { label: "x".repeat(41) } })), null, "parseNavigation rejects >40 char label");
  equal(
    parseNavigation(navWithI18n({ "zh-TW": { label: "聯絡人", extra: "no" } })),
    null,
    "parseNavigation rejects extra keys",
  );

  // 4. app-chat/v1 schema lockstep, validation bounds, and localizeAppChat.
  const fs = require("fs");
  const path = require("path");
  const schemaPath = path.join(require("process").cwd(), "..", "contracts", "app-chat", "v1", "app-chat.schema.json");
  const schema = JSON.parse(fs.readFileSync(schemaPath, "utf8"));
  equal(schema.$defs.labelI18n.maxProperties, 8, "schema labelI18n maxProperties is 8");
  equal(schema.$defs.contextI18n.maxProperties, 8, "schema contextI18n maxProperties is 8");
  equal(schema.$defs.cardI18n.maxProperties, 8, "schema cardI18n maxProperties is 8");

  const validChat = {
    contract: APP_CHAT_CONTRACT,
    app: "crm",
    contexts: [
      {
        id: "contacts",
        label: "Contacts",
        prompts: ["Summarize follow-ups"],
        i18n: {
          "zh-HK": {
            label: "聯絡人列表",
            prompts: ["總結待跟進聯絡人"],
          },
        },
        record: {
          label: "Contact",
          prompts: ["Draft follow-up"],
          i18n: {
            "zh-TW": {
              label: "聯絡人",
              prompts: ["草擬跟進訊息"],
            },
          },
        },
      },
    ],
    attachments: [
      {
        id: "csv-import",
        label: "Import CSV",
        i18n: { "zh-TW": { label: "匯入 CSV" } },
      },
    ],
    directives: [
      {
        match: "cadence_crm_contact",
        card: {
          title: "Contact update",
          text: "Saved details",
          i18n: {
            "zh-TW": { title: "聯絡人更新", text: "已儲存詳情" },
          },
          fields: [
            {
              label: "Stage",
              from: "stage",
              i18n: { "zh-TW": { label: "階段" } },
            },
          ],
          buttons: [
            {
              label: "Open contact",
              run: "open-view",
              view: "contact-detail",
              i18n: { "zh-TW": { label: "開啟聯絡人" } },
            },
          ],
        },
      },
    ],
    subjects: [
      {
        kind: "contact",
        label: "Contact",
        i18n: { "zh-TW": { label: "聯絡人" } },
      },
    ],
  };

  const parsedChat = parseAppChat(validChat);
  equal(parsedChat.contexts[0].i18n?.["zh-HK"]?.label, "聯絡人列表", "parsed chat retains context i18n");

  const chatDictT = (key: string) =>
    ({
      "Contacts": "FALLBACK-Contacts",
      "Import CSV": "FALLBACK-CSV",
      "Stage": "FALLBACK-Stage",
    })[key] ?? key;

  const localizedTw = localizeAppChat(parsedChat, "zh-TW", chatDictT);
  equal(localizedTw.contexts[0].label, "聯絡人列表", "localizeAppChat falls back from zh-HK to zh-TW");
  equal(localizedTw.contexts[0].prompts, ["總結待跟進聯絡人"], "localizeAppChat translates context prompts");
  equal(localizedTw.contexts[0].record?.label, "聯絡人", "localizeAppChat translates record label");
  equal(localizedTw.contexts[0].record?.prompts, ["草擬跟進訊息"], "localizeAppChat translates record prompts");
  equal(localizedTw.attachments[0].label, "匯入 CSV", "localizeAppChat translates attachment label");
  equal(localizedTw.subjects[0].label, "聯絡人", "localizeAppChat translates subject label");
  const card = localizedTw.directives[0].card;
  assert(card !== null, "card directive present");
  equal(card.title, "聯絡人更新", "localizeAppChat translates card title");
  equal(card.text, "已儲存詳情", "localizeAppChat translates card text");
  equal(card.fields[0].label, "階段", "localizeAppChat translates field label");
  equal(card.buttons[0].label, "開啟聯絡人", "localizeAppChat translates button label");

  const fallbackChat = parseAppChat({
    contract: APP_CHAT_CONTRACT,
    app: "crm",
    contexts: [{ id: "contacts", label: "Contacts", prompts: [] }],
    attachments: [{ id: "csv-import", label: "Import CSV" }],
  });
  const localizedFallback = localizeAppChat(fallbackChat, "zh-TW", chatDictT);
  equal(localizedFallback.contexts[0].label, "FALLBACK-Contacts", "chat context falls back to host dict");
  equal(localizedFallback.attachments[0].label, "FALLBACK-CSV", "chat attachment falls back to host dict");
  const englishChat = localizeAppChat(parsedChat, "en", chatDictT);
  equal(englishChat.contexts[0].label, "Contacts", "English preserves base descriptor strings");

  // Negative bounds on app-chat/v1 i18n
  refusedChat(
    {
      ...validChat,
      attachments: [{ id: "csv-import", label: "Import CSV", i18n: {} }],
    },
    "empty i18n map",
  );
  refusedChat(
    {
      ...validChat,
      attachments: [{ id: "csv-import", label: "Import CSV", i18n: { zh_TW: { label: "匯入" } } }],
    },
    "invalid locale tag in attachment i18n",
  );
  refusedChat(
    {
      ...validChat,
      attachments: [
        {
          id: "csv-import",
          label: "Import CSV",
          i18n: {
            en: { label: "1" },
            fr: { label: "2" },
            de: { label: "3" },
            es: { label: "4" },
            ja: { label: "5" },
            ko: { label: "6" },
            pt: { label: "7" },
            it: { label: "8" },
            nl: { label: "9" },
          },
        },
      ],
    },
    ">8 locales in attachment i18n",
  );
  refusedChat(
    {
      ...validChat,
      contexts: [
        {
          id: "contacts",
          label: "Contacts",
          prompts: [],
          i18n: { "zh-TW": { prompts: ["a", "b", "c", "d"] } },
        },
      ],
    },
    ">3 prompts in context i18n",
  );
  refusedChat(
    {
      ...validChat,
      contexts: [
        {
          id: "contacts",
          label: "Contacts",
          prompts: [],
          i18n: { "zh-TW": { prompts: ["x".repeat(81)] } },
        },
      ],
    },
    ">80 char prompt in context i18n",
  );
  refusedChat(
    {
      ...validChat,
      subjects: [
        {
          kind: "contact",
          label: "Contact",
          i18n: { "zh-TW": { label: "bad\nline" } },
        },
      ],
    },
    "control char in subject i18n",
  );
  refusedChat(
    {
      ...validChat,
      subjects: [
        {
          kind: "contact",
          label: "Contact",
          i18n: { "zh-TW": { label: "聯絡人", extra: "bad" } },
        },
      ],
    },
    "unknown key in subject i18n",
  );

  console.log("app locale checks passed");
}

main();
