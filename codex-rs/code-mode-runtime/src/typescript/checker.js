// Fork addition: type-checks `run_code` programs. Runs in the checker's own
// isolate after typescript.js (the global `ts`) and ts-blank-space.js; the
// lib files come as the JSON string `__codeModeLibs`. Rust calls
// `__codeModeCheck(sdk, source)` and gets one JSON string back:
//   {"js": "..."}                 the program passed; its types blanked out
//   {"diagnostics": [...]}        it did not; `file` is "code" or null
//   {"sdkErrorLines": [0, ...]}   the declarations themselves do not parse
(() => {
  "use strict";
  const ts = globalThis.ts;
  const { blankSourceFile } = globalThis.tsBlankSpace;
  const PROGRAM = "/code.ts";
  const SDK = "/sdk.d.ts";
  const LIB_DIR = "/lib/";
  const options = {
    target: ts.ScriptTarget.ES2025,
    lib: ["lib.es2025.d.ts"],
    module: ts.ModuleKind.ESNext,
    moduleDetection: ts.ModuleDetectionKind.Force,
    strict: false,
    noImplicitAny: false,
    strictNullChecks: false,
    erasableSyntaxOnly: true,
    allowUnreachableCode: true,
    allowUnusedLabels: true,
    types: [],
    skipLibCheck: true,
    noEmit: true,
  };
  const libTexts = JSON.parse(globalThis.__codeModeLibs);
  delete globalThis.__codeModeLibs;
  // Lib files never change: parsed once for the isolate's lifetime.
  const libFiles = new Map();
  // Declarations, parsed once per text (the latest few catalogs), and
  // whether they parse.
  const MAX_SDK_FILES = 8;
  const sdkFiles = new Map();
  let oldProgram;

  function libFile(fileName, languageVersion) {
    const name = fileName.slice(LIB_DIR.length);
    let file = libFiles.get(name);
    if (file === undefined && Object.hasOwn(libTexts, name)) {
      file = ts.createSourceFile(fileName, libTexts[name], languageVersion);
      libFiles.set(name, file);
    }
    return file;
  }

  function sdkEntry(text, languageVersion) {
    let entry = sdkFiles.get(text);
    if (entry === undefined) {
      entry = {
        file: ts.createSourceFile(SDK, text, languageVersion),
        errorLines: undefined,
      };
      if (sdkFiles.size >= MAX_SDK_FILES) {
        sdkFiles.delete(sdkFiles.keys().next().value);
      }
    } else {
      sdkFiles.delete(text);
    }
    sdkFiles.set(text, entry);
    return entry;
  }

  function position(file, start) {
    const { line, character } = file.getLineAndCharacterOfPosition(start);
    return { line: line + 1, column: character + 1 };
  }

  function diagnostic(d, programFile) {
    const file = d.file === programFile ? "code" : null;
    const at =
      file && d.start !== undefined
        ? position(programFile, d.start)
        : { line: 0, column: 0 };
    return {
      file,
      line: at.line,
      column: at.column,
      length: d.length ?? 0,
      code: d.code,
      message: ts.flattenDiagnosticMessageText(d.messageText, "\n"),
    };
  }

  function isError(d) {
    return d.category === ts.DiagnosticCategory.Error;
  }

  globalThis.__codeModeCheck = function check(sdkText, source) {
    let programFile;
    let sdk;
    const host = {
      getSourceFile(fileName, languageVersion) {
        if (fileName === PROGRAM) {
          programFile ??= ts.createSourceFile(
            PROGRAM,
            source,
            languageVersion,
            /* setParentNodes */ false,
            ts.ScriptKind.TS,
          );
          return programFile;
        }
        if (fileName === SDK) {
          sdk ??= sdkEntry(sdkText, languageVersion);
          return sdk.file;
        }
        if (fileName.startsWith(LIB_DIR)) return libFile(fileName, languageVersion);
        return undefined;
      },
      getDefaultLibFileName: () => LIB_DIR + "lib.es2025.d.ts",
      getDefaultLibLocation: () => LIB_DIR.slice(0, -1),
      writeFile() {},
      getCurrentDirectory: () => "/",
      getCanonicalFileName: (fileName) => fileName,
      useCaseSensitiveFileNames: () => true,
      getNewLine: () => "\n",
      fileExists: (fileName) =>
        fileName === PROGRAM ||
        fileName === SDK ||
        (fileName.startsWith(LIB_DIR) &&
          Object.hasOwn(libTexts, fileName.slice(LIB_DIR.length))),
      readFile: () => undefined,
      directoryExists: () => true,
      getDirectories: () => [],
    };
    const program = ts.createProgram({
      rootNames: [PROGRAM, SDK],
      options,
      host,
      oldProgram,
    });
    oldProgram = program;
    sdk ??= sdkEntry(sdkText, options.target);

    if (sdk.errorLines === undefined) {
      sdk.errorLines = program
        .getSyntacticDiagnostics(sdk.file)
        .filter(isError)
        .map((d) => sdk.file.getLineAndCharacterOfPosition(d.start ?? 0).line);
    }
    if (sdk.errorLines.length > 0) {
      return JSON.stringify({ sdkErrorLines: sdk.errorLines });
    }

    let errors = [
      ...program.getOptionsDiagnostics(),
      ...program.getGlobalDiagnostics(),
      ...program.getSyntacticDiagnostics(programFile),
    ].filter(isError);
    // Like tsc, semantic errors only for a program that parses.
    if (errors.length === 0) {
      errors = program.getSemanticDiagnostics(programFile).filter(isError);
    }
    if (errors.length > 0) {
      return JSON.stringify({
        diagnostics: errors.map((d) => diagnostic(d, programFile)),
      });
    }

    const unsupported = [];
    const js = blankSourceFile(programFile, (node) => unsupported.push(node));
    if (unsupported.length > 0) {
      return JSON.stringify({
        diagnostics: unsupported.map((node) => ({
          file: "code",
          ...position(programFile, node.getStart(programFile)),
          length: node.getWidth(programFile),
          code: 0,
          message:
            "This syntax cannot be erased to JavaScript; write erasable TypeScript only.",
        })),
      });
    }
    return JSON.stringify({ js });
  };
})();
