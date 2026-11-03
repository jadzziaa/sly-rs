/* IOP conformance module for sly-rs: exercises the IOP kernel services the
 * game's sound driver uses (threads and their priorities, semaphores,
 * event flags, delays, interrupt suspension, sysclib) and prints what happens
 * through the IOP's printf, which PCSX2 logs and sly-rs captures. Output
 * depends only on the kernel's rules, never on timing. Hardware timers are
 * not here: the BIOS's TIMRMAN lacks the functions 989snd uses (they come
 * with the game's IOPRP image), so PCSX2 cannot be the reference for them. */
#include <irx.h>
#include <loadcore.h>
#include <intrman.h>
#include <stdio.h>
#include <sysclib.h>
#include <thbase.h>
#include <thevent.h>
#include <thsemap.h>

IRX_ID("iopconf", 1, 1);

static int sema, flag;

static void worker(void *arg)
{
    int n = (int)arg;
    printf("worker %d start\n", n);
    WaitSema(sema);
    printf("worker %d has the semaphore\n", n);
    SetEventFlag(flag, 1 << n);
    printf("worker %d exits\n", n);
}

static int spawn(int priority, int n)
{
    iop_thread_t t;
    t.attr = TH_C;
    t.option = 0;
    t.thread = worker;
    t.stacksize = 0x800;
    t.priority = priority;
    int id = CreateThread(&t);
    StartThread(id, (void *)n);
    return id;
}

static void main_thread(void *arg)
{
    char buf[64];
    u32 bits = 0;
    int state;
    iop_sema_t s;
    iop_event_t e;
    (void)arg;

    printf("iop conformance 1\n");
    sprintf(buf, "%d %x %s %c", -42, 0xbeef, "ok", 'z');
    printf("sprintf [%s]\n", buf);
    printf("strtol %d %d\n", (int)strtol("-1234", NULL, 10), (int)strtol("7f", NULL, 16));
    memset(buf, 'a', 8);
    buf[8] = 0;
    memcpy(buf + 2, "XY", 2);
    printf("memset memcpy [%s] strncmp %d %d\n", buf, strncmp("abc", "abd", 2), strncmp("abc", "abd", 3) < 0);

    s.attr = 0;
    s.option = 0;
    s.initial = 0;
    s.max = 2;
    sema = CreateSema(&s);
    e.attr = 0;
    e.option = 0;
    e.bits = 0;
    flag = CreateEventFlag(&e);
    printf("poll empty %d\n", PollSema(sema));

    printf("start worker 1 (higher priority)\n");
    spawn(40, 1);
    printf("start worker 2 (lower priority)\n");
    spawn(60, 2);
    printf("signal\n");
    SignalSema(sema);
    printf("delay\n");
    DelayThread(2000);
    printf("signal again\n");
    SignalSema(sema);
    printf("wait for both\n");
    WaitEventFlag(flag, 6, WEF_AND, &bits);
    printf("flags %x\n", (unsigned int)bits);
    ClearEventFlag(flag, ~2);
    PollEventFlag(flag, 6, WEF_OR, &bits);
    printf("flags after clear %x\n", (unsigned int)bits);

    printf("suspend %d\n", CpuSuspendIntr(&state));
    printf("resume %d\n", CpuResumeIntr(state));
    DelayThread(1000); /* let worker 2 finish first */
    printf("iop done\n");
}

int _start(int argc, char *argv[])
{
    iop_thread_t t;
    (void)argc;
    (void)argv;
    t.attr = TH_C;
    t.option = 0;
    t.thread = main_thread;
    t.stacksize = 0x1000;
    t.priority = 50;
    StartThread(CreateThread(&t), NULL);
    return MODULE_RESIDENT_END;
}
