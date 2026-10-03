# K3 collaboration guidelines

## Communication

- Always reply to the user in Chinese.
- Technical terms, commands, code identifiers and original errors may remain in English, with Chinese explanations where needed.

## Project language

- English is the primary language for the project, new documentation and developer instructions. The root `README.md` is the English entry point.
- Keep translated documentation in separate language versions (`zh-Hans` and `zh-Hant`); write each translation in its target language.
- All user-visible CLI, TUI, installer, download bootstrap and runtime text must be English, including help, confirmations, status, progress, warnings and errors.
- Only UIs with i18n support may display non-English interface text through localization resources. Other interfaces and scripts must not hardcode non-English messages.
- Preserve user data and original external content, including song titles, lyrics and filenames, in their original language.
- When changing installation or runtime flows, check their entry points and invoked scripts for compliance with these language rules.

## UI component tests

- Do not add tests for UI components.
