// SPDX-FileCopyrightText: MailCraft contributors (ohos port)
// SPDX-License-Identifier: MIT OR Apache-2.0
// The entry UIAbility. Loads the ArkUI page; the native side (libentry.so ->
// libmailcraft_ohos_bridge.a -> MailCraft core) is exercised from the page via
// entry.runCommand(cmd, params). Kept minimal: the first goal is to prove the
// ArkUI <-> NAPI <-> C ABI <-> Rust core path end-to-end.
import { AbilityConstant, Configuration, UIAbility, Want } from '@kit.AbilityKit';
import { window } from '@kit.ArkUI';
import { BusinessError } from '@kit.BasicServicesKit';

export default class EntryAbility extends UIAbility {
  onCreate(want: Want, _launchParam: AbilityConstant.LaunchParam): void {
    // Cold-start "open with" carries a file/URL URI in the want; stash it for the page.
    if (want?.uri) {
      AppStorage.setOrCreate('mailcraftOpenWant', want.uri);
    }
    console.info('MailCraft ability created');
  }

  onNewWant(want: Want): void {
    if (want?.uri) {
      AppStorage.setOrCreate('mailcraftOpenWant', want.uri);
    }
  }

  onConfigurationUpdate(_newConfig: Configuration): void {
    // no-op for now
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    windowStage.loadContent('pages/Index', (error: BusinessError) => {
      if (error.code) {
        console.error(`MailCraft page failed: ${error.code} ${error.message}`);
      } else {
        console.info('MailCraft page loaded');
      }
    });
  }
}
