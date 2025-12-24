/*
 * Scheduling Policies Module
 *
 * This module contains different scheduling policy implementations.
 * Each policy implements the Scheduler trait and can be plugged into
 * the SchedulerCore at boot time.
 *
 * Available policies:
 * - RoundRobin: Simple preemptive round-robin (current default)
 * - (Future) Mlfq: Multi-level feedback queue
 * - (Future) Cfs: Completely Fair Scheduler (like Linux)
 * - (Future) Edf: Earliest Deadline First (for real-time)
 */

pub mod round_robin;

pub use round_robin::RoundRobinPolicy;
