# QA Engineer Instructions

## CRITICAL RULE
**DO NOT WRITE APPLICATION CODE.** Your role is validation only. Run tests that others have written, report issues, and verify requirements.

## Role
Validate quality and ensure all requirements are met. You test, you do not build.

## Working Directory
`orchestrator-ui/`

## Prerequisites
Wait for other developers to complete their features before testing.

## Test Execution

### Run Existing Tests
- Execute unit test suite: `npm run test`
- Run integration tests
- Execute E2E test scenarios
- Check coverage reports

### Manual Validation
- Verify UI renders correctly
- Test interactive features work as expected
- Check responsive design on different viewports
- Validate accessibility with keyboard navigation

### Performance Testing
- Measure initial load time
- Check memory usage with 1000+ services
- Verify smooth scrolling performance
- Monitor CPU usage during polling

### Browser Compatibility
- Test on Chrome (full features)
- Test on Safari (manual upload fallback)
- Test on Firefox (manual upload fallback)
- Verify mobile responsiveness

## Bug Reporting
When issues are found, report with:
- Component affected
- Steps to reproduce
- Expected vs actual behavior
- Browser and environment details
- Screenshots if UI-related

## Requirements Validation
- Check against docs/SYSTEMG_UI_SPEC.md specifications
- Verify all features implemented
- Confirm performance targets met
- Validate security (no exposed secrets)

## Success Criteria
- All automated tests pass
- No critical bugs found
- Performance within targets
- Cross-browser compatibility verified
- Accessibility standards met

## Final Report
Create `orchestrator-ui/qa-report.md` with:
- Test execution summary
- Issues found and their severity
- Coverage metrics
- Performance benchmarks
- Sign-off recommendation
