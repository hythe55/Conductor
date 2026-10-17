//! Starts the agent inside the signed-in (or sign-in screen) console session.
//!
//! The service runs in session 0, which has no access to the screen. The agent
//! is started with a copy of the service's SYSTEM token moved into the console
//! session, so it can capture and control both the normal desktop and the
//! secure desktop (lock screen, UAC prompts).

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use anyhow::{Context, Result};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_TIMEOUT};
use windows::Win32::Security::{
    DuplicateTokenEx, SecurityImpersonation, SetTokenInformation, TOKEN_ALL_ACCESS, TokenPrimary, TokenSessionId,
};
use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, GetCurrentProcess,
    GetExitCodeProcess, OpenProcessToken, PROCESS_INFORMATION, ResumeThread, STARTUPINFOW, TerminateProcess,
    WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};

/// The session attached to the physical console, if any.
pub fn console_session() -> Option<u32> {
    let id = unsafe { WTSGetActiveConsoleSessionId() };
    (id != u32::MAX).then_some(id)
}

fn wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}

/// Kills every process assigned to it when the last handle closes, so the
/// agent never outlives the service, even if the service crashes.
pub struct Job(HANDLE);

unsafe impl Send for Job {}
unsafe impl Sync for Job {}

impl Job {
    pub fn new() -> Result<Self> {
        unsafe {
            let job = CreateJobObjectW(None, PCWSTR::null()).context("CreateJobObject")?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const c_void,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .context("SetInformationJobObject")?;
            Ok(Self(job))
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        unsafe { _ = CloseHandle(self.0) };
    }
}

pub struct Child {
    process: HANDLE,
    pub session: u32,
    pub pid: u32,
}

unsafe impl Send for Child {}

impl Child {
    pub fn is_running(&self) -> bool {
        unsafe { WaitForSingleObject(self.process, 0) == WAIT_TIMEOUT }
    }

    pub fn exit_code(&self) -> Option<u32> {
        let mut code = 0;
        unsafe { GetExitCodeProcess(self.process, &mut code).ok()? };
        Some(code)
    }

    pub fn kill(&self) {
        unsafe { _ = TerminateProcess(self.process, 1) };
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        unsafe { _ = CloseHandle(self.process) };
    }
}

/// Runs `exe args` as SYSTEM inside `session`, on the interactive desktop.
pub fn spawn_in_session(job: &Job, session: u32, exe: &Path, args: &str) -> Result<Child> {
    unsafe {
        let mut own = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &mut own).context("OpenProcessToken")?;
        let mut token = HANDLE::default();
        let dup = DuplicateTokenEx(own, TOKEN_ALL_ACCESS, None, SecurityImpersonation, TokenPrimary, &mut token);
        _ = CloseHandle(own);
        dup.context("DuplicateTokenEx")?;
        let _token_guard = HandleGuard(token);

        SetTokenInformation(token, TokenSessionId, &session as *const u32 as *const c_void, size_of::<u32>() as u32)
            .context("moving token to the console session")?;

        let mut env: *mut c_void = std::ptr::null_mut();
        CreateEnvironmentBlock(&mut env, Some(token), false).context("CreateEnvironmentBlock")?;

        let mut desktop = wide(r"winsta0\default");
        let si = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            lpDesktop: PWSTR(desktop.as_mut_ptr()),
            ..Default::default()
        };
        let exe_w = wide(exe);
        let mut cmdline = wide(format!("\"{}\" {args}", exe.display()));
        let mut pi = PROCESS_INFORMATION::default();
        let created = CreateProcessAsUserW(
            Some(token),
            PCWSTR(exe_w.as_ptr()),
            Some(PWSTR(cmdline.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW | CREATE_SUSPENDED,
            Some(env),
            PCWSTR::null(),
            &si,
            &mut pi,
        );
        _ = DestroyEnvironmentBlock(env);
        created.context("CreateProcessAsUser")?;

        if let Err(e) = AssignProcessToJobObject(job.0, pi.hProcess) {
            _ = TerminateProcess(pi.hProcess, 1);
            _ = CloseHandle(pi.hThread);
            _ = CloseHandle(pi.hProcess);
            return Err(e).context("AssignProcessToJobObject");
        }
        ResumeThread(pi.hThread);
        _ = CloseHandle(pi.hThread);
        Ok(Child { process: pi.hProcess, session, pid: pi.dwProcessId })
    }
}

struct HandleGuard(HANDLE);

impl Drop for HandleGuard {
    fn drop(&mut self) {
        unsafe { _ = CloseHandle(self.0) };
    }
}
