import "@testing-library/jest-dom/vitest";
// React 18+ requires this flag in test environments that use `act`.
// Without it, `act` from React logs a warning and the updates flush
// asynchronously, leading to flaky tests.
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
