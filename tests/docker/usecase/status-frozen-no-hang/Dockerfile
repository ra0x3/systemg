FROM sysg-usecase-base

COPY tests/docker/usecase/status-frozen-no-hang/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/status-frozen-no-hang/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
