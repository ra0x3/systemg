FROM sysg-usecase-base

COPY tests/docker/usecase/status-all-services-shown/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/status-all-services-shown/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
